//! The favorites file: one server per line, `name|host|port`, next to the
//! launcher exe (`favorites.txt`). Lines starting with `#` and blank lines
//! are ignored; a malformed line is skipped instead of failing the load.

/// One saved server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    pub name: String,
    pub host: String,
    pub port: u16,
}

impl Favorite {
    pub fn new(name: &str, host: &str, port: u16) -> Self {
        Self { name: name.to_owned(), host: host.to_owned(), port }
    }
}

/// Parse the file's contents. Never fails outright: bad lines are skipped.
pub fn parse(text: &str) -> Vec<Favorite> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('|');
        let (Some(name), Some(host), Some(port)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(port) = port.trim().parse::<u16>() else { continue };
        if name.trim().is_empty() || host.trim().is_empty() {
            continue;
        }
        out.push(Favorite::new(name.trim(), host.trim(), port));
    }
    out
}

/// Serialize back to the file format.
pub fn serialize(favs: &[Favorite]) -> String {
    let mut out = String::new();
    for f in favs {
        out.push_str(&format!("{}|{}|{}\n", f.name, f.host, f.port));
    }
    out
}

/// The list a fresh install starts with.
pub fn defaults() -> Vec<Favorite> {
    vec![Favorite::new("MC-RP", "play.uifserver.net", 7776)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_skips_bad_lines() {
        let text = "# MC-RP favorites\nMC-RP|play.uifserver.net|7776\n\nbroken line\nLocal|127.0.0.1|7777\nBadPort|example.com|abc\n";
        let favs = parse(text);
        assert_eq!(favs, vec![
            Favorite::new("MC-RP", "play.uifserver.net", 7776),
            Favorite::new("Local", "127.0.0.1", 7777),
        ]);
        assert_eq!(serialize(&favs), "MC-RP|play.uifserver.net|7776\nLocal|127.0.0.1|7777\n");
    }
}
