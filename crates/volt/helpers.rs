use super::{anyhow, config::Server, Client, Result, VoltConfig};

pub fn create_client(config: &mut VoltConfig) -> Result<Client> {
    config.load_servers()?;
    Ok(Client::builder().build()?)
}

pub fn parse_server(line: &str) -> Result<Server> {
    let line = line.trim();
    if line.is_empty() {
        return Err(anyhow!("Empty server line"));
    }

    let (tls_prefix, rest) = line.split_once("://").unwrap_or(("", line));
    let tls = tls_prefix == "tls";
    let rest = if tls { rest } else { line };

    let (token, address) = rest.split_once('@').map_or((None, rest), |(t, a)| (Some(t), a));

    Ok(Server {
        tls,
        address: address.to_string(),
        token: token.map(ToString::to_string),
    })
}

pub fn format_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["b", "kb", "mb", "gb"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    match unit_index {
        0 => format!("{:.0}{}", size, UNITS[unit_index]),
        _ => format!("{:.1}{}", size, UNITS[unit_index]),
    }
}
