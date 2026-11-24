//! The WebAssembly runtime: `wasmi` (a pure-Rust interpreter) with fuel
//! metering, memory limits and a two-function host ABI.
//!
//! See `docs/plugins.md` for why wasmi (measured against wasmtime).

use std::sync::atomic::{AtomicUsize, Ordering};

use ferrocord_plugin_sdk::abi::{self, ABI_VERSION, IMPORT_MODULE, err};
use wasmi::{
    Caller, CompilationMode, Config, Engine, Extern, Linker, Memory, Module, Store, TrapCode,
    TypedFunc,
};

use super::hostcalls::{self, CallKind, Env};

/// Linear memory one plugin may use.
pub const MAX_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// Largest hook output (`fc_call` result) and host-call result.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// Fuel for instantiation plus `on_load`.
pub const LOAD_FUEL: u64 = 50_000_000;
/// Fuel per observer event (~10-50 ms of interpretation at most).
pub const EVENT_FUEL: u64 = 50_000_000;
/// Fuel for the outgoing transform: the user is waiting for the send.
pub const TRANSFORM_FUEL: u64 = 20_000_000;
/// Fuel for commands, actions and HTTP answers.
pub const ACTION_FUEL: u64 = 100_000_000;

/// Engines created in this process: zero while no plugin is enabled (the
/// zero-plugin startup test reads it).
static ENGINES: AtomicUsize = AtomicUsize::new(0);

pub fn engines_created() -> usize {
    ENGINES.load(Ordering::Relaxed)
}

/// Why a call failed. Every variant counts as a fault of the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    OutOfFuel,
    /// Trap (a Rust panic is an `unreachable` trap), stack overflow, ...
    Trap(String),
    /// Broken ABI use: missing exports, out-of-bounds buffers, oversized
    /// output, wrong ABI version.
    Abi(String),
    /// The host or interpreter panicked while running the plugin.
    Panic,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfFuel => f.write_str("ran out of its time budget"),
            Self::Trap(t) => write!(f, "crashed ({t})"),
            Self::Abi(t) => write!(f, "broke the plugin ABI ({t})"),
            Self::Panic => f.write_str("crashed the plugin host"),
        }
    }
}

fn fault(e: &wasmi::Error) -> Fault {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => Fault::OutOfFuel,
        Some(t) => Fault::Trap(t.to_string()),
        None => Fault::Trap(e.to_string()),
    }
}

/// The shared engine. Lazy translation: function bodies are validated up
/// front but compiled to wasmi bytecode on first call, so unused code costs
/// no memory.
pub fn engine() -> Engine {
    ENGINES.fetch_add(1, Ordering::Relaxed);
    let mut c = Config::default();
    c.consume_fuel(true)
        .compilation_mode(CompilationMode::LazyTranslation)
        .wasm_multi_memory(false)
        .wasm_custom_page_sizes(false)
        .floats(true)
        .set_max_recursion_depth(1024);
    Engine::new(&c)
}

/// Compiles (validates) a module.
pub fn compile(engine: &Engine, wasm: &[u8]) -> Result<Module, String> {
    Module::new(engine, wasm).map_err(|e| format!("invalid module: {e}"))
}

/// Memory limits of one plugin's store.
pub fn limits() -> wasmi::StoreLimits {
    wasmi::StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY_BYTES)
        .memories(1)
        .tables(4)
        .table_elements(100_000)
        .instances(1)
        .build()
}

/// One instantiated plugin.
pub struct Instance {
    store: Store<Env>,
    alloc: TypedFunc<i32, i32>,
    call: TypedFunc<(i32, i32, i32), i64>,
    memory: Memory,
}

fn linker(engine: &Engine) -> Result<Linker<Env>, String> {
    let mut l = Linker::<Env>::new(engine);
    l.func_wrap(
        IMPORT_MODULE,
        "host_call",
        |mut caller: Caller<'_, Env>, op: i32, ptr: i32, len: i32| -> i32 {
            let Some(mem) = memory_of(&mut caller) else {
                return err::BAD_INPUT;
            };
            let (data, env) = mem.data_and_store_mut(&mut caller);
            let Some(input) = slice(data, ptr, len) else {
                return err::BAD_INPUT;
            };
            match hostcalls::handle(env, op, input) {
                Ok(out) if out.len() <= MAX_OUTPUT_BYTES => {
                    let n = out.len() as i32;
                    env.pending = out;
                    n
                }
                Ok(_) => {
                    env.pending.clear();
                    err::LIMIT
                }
                Err(code) => {
                    env.pending.clear();
                    code
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;
    l.func_wrap(
        IMPORT_MODULE,
        "host_take",
        |mut caller: Caller<'_, Env>, ptr: i32, len: i32| -> i32 {
            let Some(mem) = memory_of(&mut caller) else {
                return err::BAD_INPUT;
            };
            let (data, env) = mem.data_and_store_mut(&mut caller);
            let n = (len.max(0) as usize).min(env.pending.len());
            let Some(dst) = slice_mut(data, ptr, n as i32) else {
                return err::BAD_INPUT;
            };
            dst.copy_from_slice(&env.pending[..n]);
            env.pending.clear();
            n as i32
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(l)
}

/// The caller's memory: cached in `Env` after instantiation.
fn memory_of(caller: &mut Caller<'_, Env>) -> Option<Memory> {
    if let Some(m) = caller.data().memory {
        return Some(m);
    }
    caller.get_export("memory").and_then(Extern::into_memory)
}

fn range(len_total: usize, ptr: i32, len: i32) -> Option<std::ops::Range<usize>> {
    let (p, l) = (ptr as u32 as usize, len as u32 as usize);
    let end = p.checked_add(l)?;
    (end <= len_total).then_some(p..end)
}

fn slice(data: &[u8], ptr: i32, len: i32) -> Option<&[u8]> {
    data.get(range(data.len(), ptr, len)?)
}

fn slice_mut(data: &mut [u8], ptr: i32, len: i32) -> Option<&mut [u8]> {
    let r = range(data.len(), ptr, len)?;
    data.get_mut(r)
}

impl Instance {
    /// Instantiates `module` (running its start function under
    /// [`LOAD_FUEL`]) and checks the ABI exports.
    pub fn new(engine: &Engine, module: &Module, env: Env) -> Result<Self, Fault> {
        let mut store = Store::new(engine, env);
        store.limiter(|e: &mut Env| &mut e.limits);
        store
            .set_fuel(LOAD_FUEL)
            .map_err(|e| Fault::Abi(e.to_string()))?;
        let linker = linker(engine).map_err(Fault::Abi)?;
        let inst = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| match e.as_trap_code() {
                Some(_) => fault(&e),
                None => Fault::Abi(e.to_string()),
            })?;
        let get = |name: &str| Fault::Abi(format!("missing export `{name}`"));
        let version: TypedFunc<(), i32> = inst
            .get_typed_func(&store, "fc_abi_version")
            .map_err(|_| get("fc_abi_version"))?;
        let alloc = inst
            .get_typed_func(&store, "fc_alloc")
            .map_err(|_| get("fc_alloc"))?;
        let call = inst
            .get_typed_func(&store, "fc_call")
            .map_err(|_| get("fc_call"))?;
        let memory = inst
            .get_memory(&store, "memory")
            .ok_or_else(|| get("memory"))?;
        store.data_mut().memory = Some(memory);
        let v = version.call(&mut store, ()).map_err(|e| fault(&e))?;
        if v != ABI_VERSION as i32 {
            return Err(Fault::Abi(format!(
                "module reports ABI {v}, expected {ABI_VERSION}"
            )));
        }
        Ok(Self {
            store,
            alloc,
            call,
            memory,
        })
    }

    pub fn env(&self) -> &Env {
        self.store.data()
    }

    pub fn env_mut(&mut self) -> &mut Env {
        self.store.data_mut()
    }

    pub fn into_env(self) -> Env {
        self.store.into_data()
    }

    /// Fuel left after the last call.
    pub fn fuel_left(&self) -> u64 {
        self.store.get_fuel().unwrap_or(0)
    }

    /// Runs hook `hook` with `payload` (MessagePack) under `fuel`. The
    /// environment's per-call state is reset to `kind` first.
    pub fn call(
        &mut self,
        hook: i32,
        payload: &[u8],
        kind: CallKind,
        fuel: u64,
    ) -> Result<Option<Vec<u8>>, Fault> {
        self.store.data_mut().begin(kind);
        self.store
            .set_fuel(fuel)
            .map_err(|e| Fault::Abi(e.to_string()))?;
        let len = i32::try_from(payload.len()).map_err(|_| Fault::Abi("payload".into()))?;
        let ptr = self
            .alloc
            .call(&mut self.store, len)
            .map_err(|e| fault(&e))?;
        let data = self.memory.data_mut(&mut self.store);
        let dst = slice_mut(data, ptr, len)
            .ok_or_else(|| Fault::Abi("fc_alloc returned an invalid buffer".into()))?;
        dst.copy_from_slice(payload);
        let r = self
            .call
            .call(&mut self.store, (hook, ptr, len))
            .map_err(|e| fault(&e))?;
        if r == 0 || r == -1 {
            // -1: the plugin could not decode the input (an SDK/ABI
            // mismatch, not a crash); treated as "no output".
            if r == -1 {
                tracing::debug!(plugin = %self.env().manifest.id, hook, "plugin rejected input");
            }
            return Ok(None);
        }
        let (p, l) = abi::unpack(r);
        if l as usize > MAX_OUTPUT_BYTES {
            return Err(Fault::Abi("output too large".into()));
        }
        let data = self.memory.data(&self.store);
        slice(data, p as i32, l as i32)
            .map(|s| Some(s.to_vec()))
            .ok_or_else(|| Fault::Abi("output out of bounds".into()))
    }
}
