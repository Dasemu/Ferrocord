//! Discord permission maths: server-wide bits from the roles, then the
//! channel overwrites (`@everyone`, roles, member), like the official
//! client. Pure functions, unit-tested.

use crate::net::model::*;

pub const CREATE_INSTANT_INVITE: u64 = 1 << 0;
pub const KICK_MEMBERS: u64 = 1 << 1;
pub const BAN_MEMBERS: u64 = 1 << 2;
pub const ADMINISTRATOR: u64 = 1 << 3;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
pub const MANAGE_GUILD: u64 = 1 << 5;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const MANAGE_ROLES: u64 = 1 << 28;
pub const ALL: u64 = u64::MAX;

/// Server-wide permissions of `me` (with `my_roles`) in `guild`: the
/// owner and Administrator get everything, everyone else the union of
/// `@everyone` (the role whose id is the guild id) and their roles.
pub fn base(guild: &Guild, me: Option<UserId>, my_roles: &[RoleId]) -> u64 {
    if me.is_some() && guild.owner_id == me {
        return ALL;
    }
    let bits = guild
        .roles
        .iter()
        .filter(|r| r.id == guild.id || my_roles.contains(&r.id))
        .fold(0, |acc, r| acc | r.permissions);
    if bits & ADMINISTRATOR != 0 { ALL } else { bits }
}

/// Applies a channel's overwrites to `base`: `@everyone` first, then all
/// of the member's roles together, then the member's own overwrite.
pub fn in_channel(
    base: u64,
    guild: GuildId,
    me: Option<UserId>,
    my_roles: &[RoleId],
    overwrites: &[PermissionOverwrite],
) -> u64 {
    if base & ADMINISTRATOR != 0 {
        return ALL;
    }
    let mut bits = base;
    if let Some(o) = overwrites.iter().find(|o| !o.member && o.id == guild) {
        bits = (bits & !o.deny) | o.allow;
    }
    let (allow, deny) = overwrites
        .iter()
        .filter(|o| !o.member && o.id != guild && my_roles.contains(&o.id))
        .fold((0, 0), |(a, d), o| (a | o.allow, d | o.deny));
    bits = (bits & !deny) | allow;
    if let Some(me) = me
        && let Some(o) = overwrites.iter().find(|o| o.member && o.id == me)
    {
        bits = (bits & !o.deny) | o.allow;
    }
    bits
}

/// Whether the permissions of `guild` can be worked out: the role list
/// holds `@everyone`. Partial guilds (mock data, a server that is still
/// loading) have none, and are then treated as fully visible.
pub fn known(guild: &Guild) -> bool {
    guild.roles.iter().any(|r| r.id == guild.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: GuildId = Snowflake(10);
    const ME: UserId = Snowflake(1);
    const MOD: RoleId = Snowflake(20);
    const MUTED: RoleId = Snowflake(21);

    fn role(id: RoleId, permissions: u64) -> Role {
        Role {
            id,
            name: String::new(),
            color: 0,
            position: 0,
            permissions,
        }
    }

    fn guild(owner: Option<UserId>, roles: Vec<Role>) -> Guild {
        Guild {
            id: G,
            name: "g".into(),
            icon: None,
            roles,
            owner_id: owner,
            emojis: vec![],
            stickers: vec![],
            premium_tier: 0,
        }
    }

    fn ow(id: Snowflake, member: bool, allow: u64, deny: u64) -> PermissionOverwrite {
        PermissionOverwrite {
            id,
            member,
            allow,
            deny,
        }
    }

    #[test]
    fn owner_and_admin_get_everything() {
        let g = guild(Some(ME), vec![role(G, VIEW_CHANNEL)]);
        assert_eq!(base(&g, Some(ME), &[]), ALL);
        let g = guild(None, vec![role(G, 0), role(MOD, ADMINISTRATOR)]);
        assert_eq!(base(&g, Some(ME), &[MOD]), ALL);
        assert_eq!(base(&g, Some(ME), &[]), 0);
        // An admin ignores channel overwrites.
        let deny_all = [ow(G, false, 0, VIEW_CHANNEL)];
        assert_eq!(in_channel(ALL, G, Some(ME), &[MOD], &deny_all), ALL);
    }

    #[test]
    fn unknown_owner_is_not_the_owner() {
        let g = guild(None, vec![role(G, VIEW_CHANNEL)]);
        assert_eq!(base(&g, None, &[]), VIEW_CHANNEL);
    }

    #[test]
    fn roles_add_up() {
        let g = guild(
            None,
            vec![
                role(G, VIEW_CHANNEL),
                role(MOD, MANAGE_MESSAGES | KICK_MEMBERS),
            ],
        );
        let b = base(&g, Some(ME), &[MOD]);
        assert_eq!(b, VIEW_CHANNEL | MANAGE_MESSAGES | KICK_MEMBERS);
    }

    #[test]
    fn overwrite_order_everyone_roles_member() {
        let b = VIEW_CHANNEL | SEND_MESSAGES;
        // Hidden from @everyone, shown to moderators.
        let o = [
            ow(G, false, 0, VIEW_CHANNEL),
            ow(MOD, false, VIEW_CHANNEL, 0),
        ];
        assert_eq!(in_channel(b, G, Some(ME), &[], &o) & VIEW_CHANNEL, 0);
        assert_ne!(in_channel(b, G, Some(ME), &[MOD], &o) & VIEW_CHANNEL, 0);
        // A role allow beats another role's deny (allows are applied last).
        let o = [
            ow(MUTED, false, 0, SEND_MESSAGES),
            ow(MOD, false, SEND_MESSAGES, 0),
        ];
        assert_ne!(
            in_channel(b, G, Some(ME), &[MOD, MUTED], &o) & SEND_MESSAGES,
            0
        );
        // The member overwrite has the last word.
        let o = [
            ow(MOD, false, SEND_MESSAGES, 0),
            ow(ME, true, 0, SEND_MESSAGES),
        ];
        assert_eq!(in_channel(b, G, Some(ME), &[MOD], &o) & SEND_MESSAGES, 0);
        // Overwrites of other roles do not apply.
        let o = [ow(MUTED, false, 0, SEND_MESSAGES)];
        assert_eq!(in_channel(b, G, Some(ME), &[MOD], &o), b);
    }

    #[test]
    fn known_needs_everyone() {
        assert!(!known(&guild(None, vec![])));
        assert!(!known(&guild(None, vec![role(MOD, 0)])));
        assert!(known(&guild(None, vec![role(G, 0)])));
    }
}
