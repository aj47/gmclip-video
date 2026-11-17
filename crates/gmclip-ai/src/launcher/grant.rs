//! Per-run grants on the Owner path (R-AGT-5, R-OWN-3 [M1]).
//!
//! `--media` (adds `read_media`) and `--private-regions` are accepted only from an
//! interactive terminal of the logged-in user, with a re-typed one-time code. Without a
//! terminal the flag is refused outright: there is no non-interactive form at M1.
//!
//! The code comes from a [`Challenger`]: once lane A2's holder is on main it is the
//! holder's Owner challenge (`owner_check("launch_grant", …)`), and the launcher passes
//! the re-typed code on in its registration so the holder can check it. Until then
//! [`LocalChallenger`] mints it here. Honest limit (R-OWN-3): a same-user process with a
//! pty can script this.

use std::io::{BufRead, IsTerminal, Write};

/// The grants a run may ask for beyond the default scopes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct Grants {
    pub media: bool,
    pub private_regions: bool,
}

impl Grants {
    pub fn any(self) -> bool {
        self.media || self.private_regions
    }

    fn describe(self) -> String {
        let mut v = Vec::new();
        if self.media {
            v.push("see frames and hear audio of the footage (read_media)");
        }
        if self.private_regions {
            v.push("see private regions (chat, alerts, private) unmasked");
        }
        v.join("; and ")
    }
}

/// The terminal the human answers on.
pub trait Tty {
    /// Both ends are an interactive terminal.
    fn interactive(&self) -> bool;
    fn show(&mut self, text: &str);
    fn read_line(&mut self) -> Option<String>;
}

/// The process's own stdin/stderr.
#[derive(Debug, Default)]
pub struct ProcessTty;

impl Tty for ProcessTty {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
    fn show(&mut self, text: &str) {
        let mut e = std::io::stderr().lock();
        let _ = e.write_all(text.as_bytes());
        let _ = e.flush();
    }
    fn read_line(&mut self) -> Option<String> {
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s).ok()?;
        Some(s)
    }
}

/// What a grant is for: the holder binds its code to exactly these arguments.
#[derive(Clone, Debug)]
pub struct GrantRequest<'a> {
    pub project: &'a str,
    pub cut: &'a str,
    pub grants: Grants,
    pub summary: &'a str,
}

/// A code that was issued. `Debug` never shows the code.
#[derive(Clone)]
pub enum Issued {
    /// The launcher holds the code: it shows it and checks the re-typed text itself.
    Here(String),
    /// The holder showed the code elsewhere (its own terminal); the launcher passes the
    /// re-typed text on and the holder checks it.
    ShownOn(String),
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Issued::Here(_) => "Issued::Here(<redacted>)",
            Issued::ShownOn(_) => "Issued::ShownOn(..)",
        })
    }
}

/// Issues the one-time code for a grant.
pub trait Challenger {
    fn issue(&mut self, req: &GrantRequest<'_>) -> std::io::Result<Issued>;
}

/// Mints a 6-character code locally (until A2's holder issues it).
#[derive(Debug, Default)]
pub struct LocalChallenger;

impl Challenger for LocalChallenger {
    fn issue(&mut self, _req: &GrantRequest<'_>) -> std::io::Result<Issued> {
        // No 0/O/1/I: the human re-types it.
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut b = [0u8; 6];
        super::credential::fill_random(&mut b)?;
        Ok(Issued::Here(
            b.iter()
                .map(|x| ALPHABET[usize::from(*x) % ALPHABET.len()] as char)
                .collect(),
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrantError {
    /// `--media` / `--private-regions` without an interactive terminal.
    NoTty,
    /// The re-typed code did not match (or nothing was typed).
    CodeMismatch,
    Io(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::NoTty => f.write_str(
                "--media and --private-regions are Owner grants: run `gmclip agent run` from an interactive terminal",
            ),
            GrantError::CodeMismatch => f.write_str("the confirmation code did not match; nothing was granted"),
            GrantError::Io(e) => write!(f, "grant confirmation: {e}"),
        }
    }
}

/// Asks the human to confirm `grants` for the run described by `summary`. Returns the
/// confirmed code (passed on to the holder), or `None` when no grant was asked for.
pub fn confirm(
    req: &GrantRequest<'_>,
    tty: &mut dyn Tty,
    challenger: &mut dyn Challenger,
) -> Result<Option<String>, GrantError> {
    let (grants, summary) = (req.grants, req.summary);
    if !grants.any() {
        return Ok(None);
    }
    if !tty.interactive() {
        return Err(GrantError::NoTty);
    }
    let issued = challenger
        .issue(req)
        .map_err(|e| GrantError::Io(e.to_string()))?;
    let head = format!(
        "\ngmclip: this agent run asks to {}.\n  {summary}\n  The grant ends with the run.\n",
        grants.describe()
    );
    match issued {
        Issued::Here(code) => {
            tty.show(&format!(
                "{head}  To allow it, type the code {code} and press return (anything else refuses): "
            ));
            let typed = tty.read_line().unwrap_or_default();
            if typed.trim() == code {
                Ok(Some(code))
            } else {
                Err(GrantError::CodeMismatch)
            }
        }
        Issued::ShownOn(place) => {
            tty.show(&format!(
                "{head}  To allow it, type the code gmclip just printed on {place} and press return: "
            ));
            let typed = tty
                .read_line()
                .unwrap_or_default()
                .trim()
                .to_ascii_uppercase();
            if typed.is_empty() {
                Err(GrantError::CodeMismatch)
            } else {
                Ok(Some(typed))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct ScriptedTty {
        pub interactive: bool,
        pub answer: Option<String>,
        pub shown: String,
    }

    impl Tty for ScriptedTty {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn show(&mut self, text: &str) {
            self.shown.push_str(text);
        }
        fn read_line(&mut self) -> Option<String> {
            self.answer.take()
        }
    }

    pub struct Fixed(pub &'static str);
    impl Challenger for Fixed {
        fn issue(&mut self, _: &GrantRequest<'_>) -> std::io::Result<Issued> {
            Ok(Issued::Here(self.0.to_owned()))
        }
    }

    pub fn req(grants: Grants, summary: &'static str) -> GrantRequest<'static> {
        GrantRequest {
            project: "p",
            cut: "c",
            grants,
            summary,
        }
    }

    const MEDIA: Grants = Grants {
        media: true,
        private_regions: false,
    };

    #[test]
    fn no_grant_needs_no_terminal() {
        let mut tty = ScriptedTty {
            interactive: false,
            answer: None,
            shown: String::new(),
        };
        assert_eq!(
            confirm(&req(Grants::default(), "s"), &mut tty, &mut Fixed("X")),
            Ok(None)
        );
    }

    #[test]
    fn media_without_a_terminal_is_refused() {
        let mut tty = ScriptedTty {
            interactive: false,
            answer: Some("ABC234\n".into()),
            shown: String::new(),
        };
        assert_eq!(
            confirm(&req(MEDIA, "s"), &mut tty, &mut Fixed("ABC234")),
            Err(GrantError::NoTty)
        );
        assert!(tty.shown.is_empty(), "no code is shown without a terminal");
    }

    #[test]
    fn media_with_a_terminal_requires_the_code() {
        let mut tty = ScriptedTty {
            interactive: true,
            answer: Some("ABC234\n".into()),
            shown: String::new(),
        };
        assert_eq!(
            confirm(&req(MEDIA, "cut h79"), &mut tty, &mut Fixed("ABC234")),
            Ok(Some("ABC234".into()))
        );
        assert!(tty.shown.contains("read_media") && tty.shown.contains("cut h79"));
        for wrong in [Some("ABC235\n".to_owned()), Some(String::new()), None] {
            let mut tty = ScriptedTty {
                interactive: true,
                answer: wrong,
                shown: String::new(),
            };
            assert_eq!(
                confirm(&req(MEDIA, "s"), &mut tty, &mut Fixed("ABC234")),
                Err(GrantError::CodeMismatch)
            );
        }
    }

    #[test]
    fn local_codes_are_six_unambiguous_characters() {
        let Issued::Here(c) = LocalChallenger.issue(&req(MEDIA, "s")).unwrap() else {
            panic!()
        };
        assert_eq!(c.len(), 6);
        assert!(c.chars().all(|ch| !"01OI".contains(ch)), "{c}");
    }
}
