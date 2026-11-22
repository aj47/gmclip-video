//! Security descriptors as SDDL text (decisions/0019): the owner-only descriptor gmclip
//! creates its private pipes, directories and files with, and the check that an
//! existing DACL lets nobody else in. Pure string logic, so it is tested on every OS;
//! [`crate::windows`] does the FFI.
//!
//! The descriptor names the owner explicitly (`O:<sid>`): an elevated process's default
//! owner is `BUILTIN\Administrators`, which would fail the owner check afterwards. The
//! DACL is protected (`P`: nothing inherited from the parent) and has one ACE, full
//! access for the current user. SYSTEM and Administrators get no ACE; backup and
//! antivirus read through their privileges, not the DACL.

/// Is `s` a SID string as Windows prints one (`S-1-<authority>-<sub>…`)? Checked before a
/// SID is placed into SDDL, so a malformed string can never add an ACE.
pub fn is_sid(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 200
        && rest
            .split('-')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// What the descriptor is for: directories pass the ACE on to what is created inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Object {
    Directory,
    File,
    Pipe,
}

/// The owner-only descriptor for `sid`, or `None` if `sid` is not a SID string.
pub fn owner_only(sid: &str, object: Object) -> Option<String> {
    if !is_sid(sid) {
        return None;
    }
    let inherit = match object {
        Object::Directory => "OICI",
        Object::File | Object::Pipe => "",
    };
    Some(format!("O:{sid}D:P(A;{inherit};FA;;;{sid})"))
}

/// Allow-type ACEs (plain, object, callback, callback object).
const ALLOW: &[&str] = &["A", "OA", "XA", "ZA"];

/// Does the DACL in `sddl` (as `ConvertSecurityDescriptorToStringSecurityDescriptorW`
/// prints it with `DACL_SECURITY_INFORMATION`) grant access to nobody but `sid`?
///
/// Fails closed: no DACL, a NULL DACL (`NO_ACCESS_CONTROL`), an unparsable ACE, or any
/// allow ACE for another trustee (an alias such as `SY`, `BA`, `WD`, `OW` included) is
/// `false`. Deny ACEs are fine. An empty DACL (nobody at all) is `true`.
pub fn dacl_private_to(sddl: &str, sid: &str) -> bool {
    dacl_private_to_resolving(sddl, sid, |_| None)
}

/// [`dacl_private_to`], where a trustee Windows printed as a two-letter SDDL alias counts
/// as `sid` when `resolve` turns the alias into exactly `sid`. Windows prints some
/// accounts only as an alias: the built-in Administrator (RID 500) is `LA`, which is
/// the user CI runners and some machines run as. Any other alias is still refused.
pub fn dacl_private_to_resolving(
    sddl: &str,
    sid: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> bool {
    if !is_sid(sid) {
        return false;
    }
    let Some(start) = sddl.find("D:") else {
        return false;
    };
    let body = &sddl[start + 2..];
    let flags_end = body.find('(').unwrap_or(body.len());
    let flags = &body[..flags_end];
    if flags.contains("NO_ACCESS_CONTROL") || flags.contains(':') {
        return false;
    }
    let mut rest = &body[flags_end..];
    while let Some(after_open) = rest.strip_prefix('(') {
        // balanced: a conditional ACE carries its own parentheses
        let mut depth = 1usize;
        let mut end = None;
        for (i, c) in after_open.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            return false;
        };
        let ace = &after_open[..end];
        let fields: Vec<&str> = ace.splitn(7, ';').collect();
        if fields.len() < 6 {
            return false;
        }
        if ALLOW.contains(&fields[0]) && fields[5] != sid && !is_alias_of(fields[5], sid, &resolve)
        {
            return false;
        }
        rest = &after_open[end + 1..];
    }
    // anything after the ACEs must be another section (`S:`), never more DACL text
    rest.is_empty() || rest.starts_with("S:")
}

/// Is `trustee` an SDDL alias (two uppercase letters) that `resolve` maps to `sid`?
fn is_alias_of(trustee: &str, sid: &str, resolve: &impl Fn(&str) -> Option<String>) -> bool {
    trustee.len() == 2
        && trustee.bytes().all(|b| b.is_ascii_uppercase())
        && resolve(trustee).as_deref() == Some(sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";

    #[test]
    fn sid_strings_are_checked_before_they_reach_sddl() {
        assert!(is_sid(ME));
        assert!(is_sid("S-1-5-18"));
        for bad in [
            "",
            "S-1-",
            "S-1-5-",
            "S-1-5--1",
            "SY",
            "S-1-5-21)(A;;FA;;;WD",
            "s-1-5-18",
        ] {
            assert!(!is_sid(bad), "{bad}");
            assert!(owner_only(bad, Object::File).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_owner_only_descriptor_names_the_owner_and_one_ace() {
        assert_eq!(
            owner_only(ME, Object::Directory).unwrap(),
            format!("O:{ME}D:P(A;OICI;FA;;;{ME})")
        );
        assert_eq!(
            owner_only(ME, Object::Pipe).unwrap(),
            format!("O:{ME}D:P(A;;FA;;;{ME})")
        );
        // what we create passes our own check
        let made = owner_only(ME, Object::File).unwrap();
        assert!(dacl_private_to(&made, ME));
    }

    #[test]
    fn a_dacl_with_anyone_else_allowed_is_not_private() {
        let ok = [
            format!("D:P(A;OICI;FA;;;{ME})"),
            format!("D:PAI(A;OICIID;FA;;;{ME})(D;;FA;;;WD)"),
            "D:P".to_owned(),
            format!("D:(A;;0x1200a9;;;{ME})S:(ML;;NW;;;ME)"),
        ];
        for s in &ok {
            assert!(dacl_private_to(s, ME), "{s}");
        }
        let bad = [
            String::new(),
            "O:SY".to_owned(),
            "D:NO_ACCESS_CONTROL".to_owned(),
            format!("D:P(A;OICI;FA;;;{ME})(A;OICI;FA;;;SY)"),
            format!("D:P(A;;FA;;;{ME})(A;;FR;;;BA)"),
            format!("D:P(A;;FA;;;{ME})(XA;;FA;;;WD;(Member_of {{SID(BA)}}))"),
            format!("D:P(A;;FA;;;{ME})(OA;;FA;guid;;AU)"),
            format!("D:P(A;;FA;;;{ME})(A;;FA;;;OW)"),
            format!("D:P(A;;FA;;;{ME}"),
            format!("D:P(A;;FA;;;{ME})junk"),
            "D:P(A;;FA)".to_owned(),
            // another user's SID, a prefix of ours
            "D:P(A;;FA;;;S-1-5-21-1004336348-1177238915-682003330-100)".to_owned(),
        ];
        for s in &bad {
            assert!(!dacl_private_to(s, ME), "{s}");
        }
        assert!(!dacl_private_to(&format!("D:P(A;;FA;;;{ME})"), "SY"));
    }

    #[test]
    fn an_alias_counts_only_when_it_resolves_to_the_user() {
        const ADMIN: &str = "S-1-5-21-3414932011-3134797172-3625386099-500";
        let la = |a: &str| (a == "LA").then(|| ADMIN.to_owned());
        let dacl = "D:P(A;OICI;FA;;;LA)";
        assert!(
            !dacl_private_to(dacl, ADMIN),
            "unresolved aliases fail closed"
        );
        assert!(dacl_private_to_resolving(dacl, ADMIN, la));
        assert!(
            !dacl_private_to_resolving(dacl, ME, la),
            "LA is someone else"
        );
        assert!(!dacl_private_to_resolving(
            "D:P(A;;FA;;;LA)(A;;FA;;;WD)",
            ADMIN,
            la
        ));
        // only two-letter aliases are resolved, never a SID string or a longer name
        let any = |_: &str| Some(ADMIN.to_owned());
        assert!(!dacl_private_to_resolving(
            "D:P(A;;FA;;;S-1-5-18)",
            ADMIN,
            any
        ));
        assert!(!dacl_private_to_resolving("D:P(A;;FA;;;LAX)", ADMIN, any));
    }
}
