//! Media plumbing that does not need a window: the wire protocol of the
//! video helper process and the code that runs it. The decoders themselves
//! live in `src/bin/ferrocord-player` so this library (and the app) link no
//! codec.

pub mod clip;
pub mod scratch;
pub mod video;
pub mod video_proto;
