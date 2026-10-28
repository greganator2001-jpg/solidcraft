//! Bambu Lab printers on your network, the way Bambu Studio talks to them in LAN mode:
//! - status and commands: MQTT 3.1.1 over TLS on port 8883 (user `bblp`, password = the access code)
//! - files: FTPS with implicit TLS on port 990, same login; the print file goes in the root folder
//!
//! The printers use their own self-signed certificates (no public authority signs them), so the
//! certificate can't be checked; the access code is what proves it's your printer.
//!
//! Bambu's newer firmware only takes print commands from other programs in LAN Only mode with Developer
//! Mode on; [`NEED_DEV_MODE`] is its answer otherwise.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::{Map, Value, json};

use crate::Result;

pub const MQTT_PORT: u16 = 8883;
pub const FTP_PORT: u16 = 990;
pub const USER: &str = "bblp";
/// What newer firmware answers to a command when Developer Mode is off.
pub const NEED_DEV_MODE: i64 = 84_033_543;
/// The H2 family (H2D, H2D Pro, H2S, H2C …): Auto bed leveling / flow dynamics, `ams_mapping2`.
pub const H2_MODELS: [&str; 5] = ["O1D", "O1E", "O1S", "O1C", "O2D"];
/// Two nozzles: nozzle offset calibration applies.
pub const DUAL_NOZZLE: [&str; 3] = ["O1D", "O1E", "O2D"];
/// Largest single MQTT packet read (status reports are tens of kB).
const MAX_PACKET: usize = 4 << 20;

pub fn is_h2(model: &str) -> bool {
    let m = model.to_uppercase();
    H2_MODELS.contains(&m.as_str()) || m.starts_with('O') || m.contains("H2")
}

// ------------------------------------------------------------------ finding printers (SSDP on UDP 2021)
/// A printer that announced itself on the network.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub ip: String,
    pub serial: String,
    /// Bambu's model code ("N2S", "O1S" …)
    pub model: String,
    pub name: String,
}

/// Read one SSDP announcement; None when it isn't a Bambu printer's.
pub fn parse_ssdp(text: &str, from_ip: &str) -> Option<Found> {
    if !text.to_lowercase().contains("bambu") {
        return None;
    }
    let mut f = Found { ip: from_ip.to_string(), ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().to_string();
        match k.trim().to_lowercase().as_str() {
            "usn" => f.serial = v,
            "location" if !v.is_empty() => f.ip = v,
            "devmodel.bambu.com" => f.model = v,
            "devname.bambu.com" => f.name = v,
            _ => {}
        }
    }
    (!f.serial.is_empty() && !f.ip.is_empty()).then_some(f)
}

/// Bambu printers on this network, found by listening (and asking) for their SSDP announcements for
/// up to `wait`; stops early once every serial in `want` has been heard.
pub fn discover(wait: Duration, want: &[String]) -> Vec<Found> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind(("0.0.0.0", 2021)).or_else(|_| UdpSocket::bind(("0.0.0.0", 0)));
    let Ok(sock) = sock else { return Vec::new() };
    sock.set_broadcast(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(300))).ok();
    let probe =
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1990\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: urn:bambulab-com:device:3dprinter:1\r\n\r\n";
    for target in ["239.255.255.250:1990", "255.255.255.255:2021", "255.255.255.255:1990"] {
        sock.send_to(probe.as_bytes(), target).ok();
    }
    let mut found: Vec<Found> = Vec::new();
    let mut buf = vec![0u8; 4096];
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if let Ok((n, from)) = sock.recv_from(&mut buf)
            && let Some(f) = parse_ssdp(&String::from_utf8_lossy(&buf[..n]), &from.ip().to_string())
            && !found.iter().any(|x| x.serial == f.serial)
        {
            found.push(f);
            if !want.is_empty() && want.iter().all(|w| found.iter().any(|x| &x.serial == w)) {
                break;
            }
        }
    }
    found
}

// ------------------------------------------------------------------ TLS without a CA (the printers' own certificates)
#[derive(Debug)]
struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// One TLS setup for a printer: the FTPS data connection must resume the control connection's session,
/// which rustls does when both use the same config (its in-memory session cache).
pub fn tls_config() -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed ({e})"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

fn tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("couldn't find {host} ({e})"))?.collect();
    let mut last = format!("couldn't reach {host}");
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                s.set_read_timeout(Some(timeout)).ok();
                s.set_write_timeout(Some(timeout)).ok();
                s.set_nodelay(true).ok();
                return Ok(s);
            }
            Err(e) => last = format!("couldn't reach the printer at {host}:{port} ({e})"),
        }
    }
    Err(last)
}

type Tls = StreamOwned<ClientConnection, TcpStream>;

fn tls(cfg: &Arc<ClientConfig>, host: &str, sock: TcpStream) -> Result<Tls> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| format!("bad printer address {host} ({e})"))?;
    let conn = ClientConnection::new(cfg.clone(), name).map_err(|e| format!("TLS to the printer failed ({e})"))?;
    Ok(StreamOwned::new(conn, sock))
}

// ------------------------------------------------------------------ MQTT 3.1.1 (the parts we need)
fn enc_len(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn mstr(s: &str, out: &mut Vec<u8>) {
    let b = s.as_bytes();
    let n = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&b[..n]);
}

fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![kind];
    enc_len(body.len(), &mut p);
    p.extend_from_slice(body);
    p
}

fn read_packet<R: Read>(r: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 1];
    r.read_exact(&mut h)?;
    let (mut n, mut mult) = (0usize, 1usize);
    for _ in 0..4 {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        n += usize::from(b[0] & 0x7f) * mult;
        if b[0] & 0x80 == 0 {
            break;
        }
        mult *= 128;
    }
    if n > MAX_PACKET {
        return Err(std::io::Error::other("packet too large"));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok((h[0], body))
}

/// A complete packet at the start of `buf`: (header, body, bytes used); None until it has all arrived.
fn take_packet(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    let h = *buf.first()?;
    let (mut n, mut mult, mut i) = (0usize, 1usize, 1usize);
    loop {
        let b = *buf.get(i)?;
        n += usize::from(b & 0x7f) * mult;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        mult *= 128;
        if i > 4 {
            return None;
        }
    }
    let body = buf.get(i..i + n)?.to_vec();
    Some((h, body, i + n))
}

/// The topic and payload of a PUBLISH packet.
fn publish_parts(h: u8, body: &[u8]) -> Option<(String, &[u8])> {
    let tl = usize::from(u16::from_be_bytes([*body.first()?, *body.get(1)?]));
    let topic = String::from_utf8_lossy(body.get(2..2 + tl)?).to_string();
    let mut i = 2 + tl;
    if (h >> 1) & 3 > 0 {
        i += 2;
    }
    Some((topic, body.get(i..)?))
}

// ------------------------------------------------------------------ what a printer reports
/// An AMS tray (or the external spool).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tray {
    /// AMS unit (0, 1 …; 128+ for AMS HT; 255 = the external spool)
    pub ams: u32,
    /// 0..3 in that unit
    pub slot: u32,
    pub material: String,
    /// '#rrggbb'
    pub color: String,
    pub name: String,
    /// Bambu's filament ID of what's loaded ('GFA00', or 'P…' for your own filament)
    pub filament_id: String,
}

impl Tray {
    /// The printer's single-number tray id (what `ams_mapping` uses).
    pub fn index(&self) -> i64 {
        if self.ams == 255 {
            254
        } else if self.ams >= 128 {
            i64::from(self.ams)
        } else {
            i64::from(self.ams) * 4 + i64::from(self.slot)
        }
    }

    pub fn label(&self) -> String {
        let place = if self.ams == 255 {
            "External spool".to_string()
        } else if self.ams >= 128 {
            format!("AMS HT {}", self.ams - 127)
        } else {
            format!("AMS {} slot {}", self.ams + 1, self.slot + 1)
        };
        let what = if !self.name.is_empty() && self.name.to_uppercase().starts_with(&self.material.to_uppercase()) {
            self.name.clone()
        } else {
            [self.material.as_str(), self.name.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ")
        };
        if what.is_empty() { format!("{place}: empty") } else { format!("{place}: {what}") }
    }
}

/// A printer's state, from its reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// IDLE, PREPARE, RUNNING, PAUSE, FINISH, FAILED
    pub state: String,
    pub percent: i64,
    pub minutes_left: i64,
    pub layer: i64,
    pub layers: i64,
    pub job: String,
    pub nozzle: f64,
    pub nozzle_target: f64,
    pub bed: f64,
    pub bed_target: f64,
    pub error: i64,
    pub trays: Vec<Tray>,
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(self.state.as_str(), "RUNNING" | "PAUSE" | "PREPARE")
    }

    pub fn summary(&self) -> String {
        let st = match self.state.as_str() {
            "IDLE" => "Idle",
            "PREPARE" => "Preparing",
            "RUNNING" => "Printing",
            "PAUSE" => "Paused",
            "FINISH" => "Finished",
            "FAILED" => "Failed",
            "" => "—",
            other => other,
        };
        if self.busy() {
            let (h, m) = (self.minutes_left / 60, self.minutes_left % 60);
            let left = if h > 0 { format!("{h}h {m:02}m left") } else { format!("{m} min left") };
            format!("{st} {}% · layer {}/{} · {left}", self.percent, self.layer, self.layers)
        } else {
            st.to_string()
        }
    }
}

fn num(p: &Map<String, Value>, k: &str) -> f64 {
    match p.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn colour(v: Option<&Value>) -> String {
    let c: String = text(v).trim().trim_start_matches('#').chars().take(6).collect();
    if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()) { format!("#{}", c.to_uppercase()) } else { String::new() }
}

fn tray_of(ams: u32, t: &Map<String, Value>) -> Tray {
    Tray {
        ams,
        slot: text(t.get("id")).parse().unwrap_or(0),
        material: text(t.get("tray_type")),
        color: colour(t.get("tray_color")),
        name: text(t.get("tray_sub_brands")),
        filament_id: text(t.get("tray_info_idx")),
    }
}

/// A printer's report (the `print` part, merged over time) as a [`Status`].
pub fn parse_status(p: &Map<String, Value>) -> Status {
    let mut s = Status {
        state: text(p.get("gcode_state")),
        percent: num(p, "mc_percent") as i64,
        minutes_left: num(p, "mc_remaining_time") as i64,
        layer: num(p, "layer_num") as i64,
        layers: num(p, "total_layer_num") as i64,
        job: text(p.get("subtask_name")),
        nozzle: num(p, "nozzle_temper"),
        nozzle_target: num(p, "nozzle_target_temper"),
        bed: num(p, "bed_temper"),
        bed_target: num(p, "bed_target_temper"),
        error: num(p, "print_error") as i64,
        trays: Vec::new(),
    };
    if let Some(units) = p.get("ams").and_then(|a| a.get("ams")).and_then(Value::as_array) {
        for u in units.iter().filter_map(Value::as_object) {
            let Ok(aid) = text(u.get("id")).parse::<u32>() else { continue };
            for t in u.get("tray").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object) {
                s.trays.push(tray_of(aid, t));
            }
        }
    }
    match p.get("vt_tray") {
        Some(Value::Object(t)) => s.trays.push(Tray { slot: 0, ..tray_of(255, t) }),
        Some(Value::Array(a)) => s.trays.extend(a.iter().filter_map(Value::as_object).map(|t| Tray { slot: 0, ..tray_of(255, t) })),
        _ => {}
    }
    s
}

fn merge(dst: &mut Map<String, Value>, src: &Map<String, Value>) {
    for (k, v) in src {
        match (dst.get_mut(k), v) {
            (Some(Value::Object(d)), Value::Object(s)) => merge(d, s),
            _ => {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Bambu Studio's print option: "auto" / "on" / "off" -> 2 / 1 / 0.
fn mode(v: &str) -> i64 {
    match v.to_lowercase().as_str() {
        "off" | "false" | "0" => 0,
        "on" | "true" | "1" => 1,
        _ => 2,
    }
}

/// Send-print choices.
#[derive(Clone, Debug)]
pub struct PrintOptions {
    pub bed_leveling: String,
    pub flow_cali: String,
    pub nozzle_offset_cali: String,
    pub timelapse: bool,
}

impl Default for PrintOptions {
    fn default() -> Self {
        Self { bed_leveling: "auto".into(), flow_cali: "auto".into(), nozzle_offset_cali: "auto".into(), timelapse: false }
    }
}

/// The `project_file` command that starts printing a `.gcode.3mf` already on the printer.
/// `mapping`: for each filament of the sliced file (slot 1, 2 …), the tray it prints from, or None.
pub fn print_payload(filename: &str, plate: u32, model: &str, mapping: &[Option<Tray>], opts: &PrintOptions, seq: u64, name: &str) -> Value {
    let h2 = is_h2(model);
    let use_ams = mapping.iter().flatten().any(|t| t.ams != 255);
    let mut flat: Vec<i64> = mapping.iter().map(|t| t.as_ref().map_or(-1, Tray::index)).collect();
    if !h2 {
        // A1 / P1 / X1 firmware reads a table of at least 5
        while flat.len() < 5 {
            flat.push(-1);
        }
    }
    let two: Vec<Value> = mapping
        .iter()
        .map(|t| match t {
            Some(t) => json!({"ams_id": t.ams, "slot_id": t.slot}),
            None => json!({"ams_id": 255, "slot_id": 255}),
        })
        .collect();
    let (mut bl, mut fc) = (mode(&opts.bed_leveling), mode(&opts.flow_cali));
    if !h2 {
        // no Auto on these: Auto means On
        bl = if bl == 2 { 1 } else { bl };
        fc = if fc == 2 { 1 } else { fc };
    }
    let mut p = json!({
        "sequence_id": seq.to_string(), "command": "project_file", "param": format!("Metadata/plate_{plate}.gcode"),
        "project_id": "0", "profile_id": "0", "task_id": "0", "subtask_id": "0", "subtask_name": name,
        "file": filename, "url": format!("ftp:///{filename}"), "md5": "",
        "timelapse": opts.timelapse, "bed_type": "auto", "bed_levelling": bl == 1, "bed_leveling": bl == 1,
        "auto_bed_leveling": bl, "flow_cali": fc == 1, "extrude_cali_flag": fc, "vibration_cali": false,
        "layer_inspect": false, "use_ams": use_ams, "ams_mapping": if use_ams { flat } else { Vec::new() }, "cfg": "0",
    });
    if let Some(o) = p.as_object_mut() {
        if DUAL_NOZZLE.contains(&model.to_uppercase().as_str()) {
            o.insert("nozzle_offset_cali".into(), Value::from(mode(&opts.nozzle_offset_cali)));
        }
        if use_ams {
            o.insert("ams_mapping2".into(), Value::from(two));
        }
    }
    json!({ "print": p })
}

/// Did the printer refuse the command?
pub fn check_reply(reply: &Map<String, Value>) -> Result<()> {
    let err = num(reply, "err_code").max(num(reply, "error_code")) as i64;
    let reason = text(reply.get("reason"));
    if err == NEED_DEV_MODE || reason.to_lowercase().contains("verify") {
        return Err("the printer won't take prints from other programs unless Developer Mode is on (printer screen: Settings › LAN Only Mode › Developer Mode). Use Send with Bambu Studio instead.".into());
    }
    if text(reply.get("result")).eq_ignore_ascii_case("fail") || err != 0 {
        return Err(format!("the printer said no ({})", if reason.is_empty() { err.to_string() } else { reason }));
    }
    Ok(())
}

// ------------------------------------------------------------------ one printer
/// A printer to talk to.
#[derive(Clone, Debug, Default)]
pub struct PrinterAddr {
    pub ip: String,
    pub serial: String,
    pub access_code: String,
    pub model: String,
}

/// A live connection: status keeps coming in on a background thread.
pub struct Printer {
    pub addr: PrinterAddr,
    tls_cfg: Arc<ClientConfig>,
    stream: Arc<Mutex<Tls>>,
    shared: Arc<Mutex<Shared>>,
    seq: u64,
}

#[derive(Default)]
struct Shared {
    report: Map<String, Value>,
    replies: Vec<Map<String, Value>>,
    alive: bool,
}

impl Printer {
    /// Connect (MQTT over TLS), subscribe to reports and ask for a full status; waits up to `wait` for it.
    pub fn connect(addr: PrinterAddr, wait: Duration) -> Result<Printer> {
        if addr.ip.trim().is_empty() || addr.serial.trim().is_empty() || addr.access_code.trim().is_empty() {
            return Err("the printer's IP address, serial number and access code are all needed".into());
        }
        let cfg = tls_config()?;
        let sock =
            tcp(addr.ip.trim(), MQTT_PORT, Duration::from_secs(8)).map_err(|e| format!("{e}. Is it on, on this network, and in LAN Only mode?"))?;
        let mut s = tls(&cfg, addr.ip.trim(), sock)?;
        let mut body = Vec::new();
        mstr("MQTT", &mut body);
        body.extend_from_slice(&[4, 0x02 | 0x80 | 0x40]);
        body.extend_from_slice(&60u16.to_be_bytes());
        mstr(&format!("fission-{}", std::process::id()), &mut body);
        mstr(USER, &mut body);
        mstr(addr.access_code.trim(), &mut body);
        s.write_all(&packet(0x10, &body)).map_err(|e| format!("couldn't log in to the printer ({e})"))?;
        let (h, ack) = read_packet(&mut s).map_err(|e| format!("the printer didn't answer the login ({e})"))?;
        if h >> 4 != 2 || ack.len() < 2 {
            return Err("the printer didn't answer like a Bambu printer".into());
        }
        match ack[1] {
            0 => {}
            4 | 5 => return Err("the printer refused the access code — check it on the printer's screen".into()),
            c => return Err(format!("the printer refused the connection (code {c})")),
        }
        let mut sub = Vec::new();
        sub.extend_from_slice(&1u16.to_be_bytes());
        mstr(&format!("device/{}/report", addr.serial.trim()), &mut sub);
        sub.push(0);
        s.write_all(&packet(0x82, &sub)).map_err(|e| format!("couldn't subscribe ({e})"))?;
        // reads time out every second so the reader can notice a close
        s.sock.set_read_timeout(Some(Duration::from_secs(1))).ok();
        let stream = Arc::new(Mutex::new(s));
        let shared = Arc::new(Mutex::new(Shared { alive: true, ..Default::default() }));
        Self::spawn_reader(stream.clone(), shared.clone());
        let p = Printer { addr, tls_cfg: cfg, stream, shared, seq: 0 };
        p.request_status()?;
        let end = Instant::now() + wait;
        while Instant::now() < end {
            if p.shared.lock().map(|s| !s.report.is_empty()).unwrap_or(false) {
                return Ok(p);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if wait.is_zero() {
            return Ok(p);
        }
        p.close();
        Err("the printer accepted the login but sent no status — check the serial number".into())
    }

    fn spawn_reader(stream: Arc<Mutex<Tls>>, shared: Arc<Mutex<Shared>>) {
        std::thread::spawn(move || {
            let mut last_ping = Instant::now();
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = vec![0u8; 64 * 1024];
            loop {
                if !shared.lock().map(|s| s.alive).unwrap_or(false) {
                    break;
                }
                // read whatever has arrived (a timed-out read loses nothing: bytes collect in `buf`)
                let got = {
                    let Ok(mut s) = stream.lock() else { break };
                    if last_ping.elapsed() > Duration::from_secs(30) {
                        s.write_all(&[0xc0, 0x00]).ok();
                        last_ping = Instant::now();
                    }
                    s.read(&mut tmp)
                };
                match got {
                    Ok(0) => {
                        if let Ok(mut sh) = shared.lock() {
                            sh.alive = false;
                        }
                        break;
                    }
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    Err(_) => {
                        if let Ok(mut sh) = shared.lock() {
                            sh.alive = false;
                        }
                        break;
                    }
                }
                if buf.len() > MAX_PACKET * 2 {
                    buf.clear(); // something's wrong with the stream: drop it rather than grow forever
                }
                while let Some((h, body, used)) = take_packet(&buf) {
                    if h >> 4 == 3
                        && let Some((_topic, payload)) = publish_parts(h, &body)
                        && let Ok(Value::Object(j)) = serde_json::from_slice::<Value>(payload)
                        && let Ok(mut sh) = shared.lock()
                    {
                        match j.get("print") {
                            Some(Value::Object(p)) => {
                                merge(&mut sh.report, p);
                                let cmd = text(p.get("command"));
                                if (!cmd.is_empty() && cmd != "push_status") || p.contains_key("result") {
                                    sh.replies.push(p.clone());
                                }
                            }
                            _ => sh.replies.push(j.clone()),
                        }
                        if sh.replies.len() > 64 {
                            sh.replies.remove(0);
                        }
                    }
                    buf.drain(..used);
                }
            }
        });
    }

    pub fn connected(&self) -> bool {
        self.shared.lock().map(|s| s.alive).unwrap_or(false)
    }

    pub fn status(&self) -> Status {
        self.shared.lock().map(|s| parse_status(&s.report)).unwrap_or_default()
    }

    fn publish(&self, payload: &Value) -> Result<()> {
        let mut body = Vec::new();
        mstr(&format!("device/{}/request", self.addr.serial.trim()), &mut body);
        body.extend_from_slice(payload.to_string().as_bytes());
        let mut s = self.stream.lock().map_err(|_| "the printer connection broke")?;
        s.write_all(&packet(0x30, &body)).map_err(|e| format!("couldn't send to the printer ({e})"))
    }

    pub fn request_status(&self) -> Result<()> {
        self.publish(&json!({"pushing": {"sequence_id": "0", "command": "pushall", "version": 1, "push_target": 1}}))
    }

    fn wait_reply(&self, command: &str, seq: &str, timeout: Duration) -> Option<Map<String, Value>> {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(sh) = self.shared.lock()
                && let Some(r) = sh.replies.iter().rev().find(|r| {
                    text(r.get("command")) == command && {
                        let s = text(r.get("sequence_id"));
                        s.is_empty() || s == seq
                    }
                })
            {
                return Some(r.clone());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    fn command(&mut self, mut payload: Value, command: &str, timeout: Duration) -> Result<()> {
        self.seq += 1;
        let seq = self.seq.to_string();
        if let Some(o) = payload.as_object_mut() {
            for v in o.values_mut() {
                if let Some(inner) = v.as_object_mut() {
                    inner.insert("sequence_id".into(), Value::from(seq.clone()));
                }
            }
        }
        self.publish(&payload)?;
        match self.wait_reply(command, &seq, timeout) {
            Some(r) => check_reply(&r),
            None => Ok(()), // older firmware doesn't always answer; the status will tell
        }
    }

    pub fn pause(&mut self) -> Result<()> {
        self.command(json!({"print": {"command": "pause", "param": ""}}), "pause", Duration::from_secs(5))
    }
    pub fn resume(&mut self) -> Result<()> {
        self.command(json!({"print": {"command": "resume", "param": ""}}), "resume", Duration::from_secs(5))
    }
    pub fn stop(&mut self) -> Result<()> {
        self.command(json!({"print": {"command": "stop", "param": ""}}), "stop", Duration::from_secs(5))
    }

    /// Upload a file to the printer's storage (FTPS), calling `progress(sent, total)`.
    pub fn upload(&self, data: &[u8], name: &str, progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
        ftps_store(&self.tls_cfg, self.addr.ip.trim(), &self.addr.access_code, name, data, progress)
    }

    /// Start printing a `.gcode.3mf` already uploaded.
    pub fn start(&mut self, filename: &str, mapping: &[Option<Tray>], opts: &PrintOptions, name: &str) -> Result<()> {
        let payload = print_payload(filename, 1, &self.addr.model, mapping, opts, self.seq + 1, name);
        self.command(payload, "project_file", Duration::from_secs(8))
    }

    pub fn close(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.alive = false;
        }
        if let Ok(mut s) = self.stream.lock() {
            s.write_all(&[0xe0, 0x00]).ok();
            s.conn.send_close_notify();
            s.flush().ok();
        }
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        self.close();
    }
}

// ------------------------------------------------------------------ FTPS (implicit TLS, session reuse)
fn ftp_reply<R: BufRead>(r: &mut R) -> Result<(u32, String)> {
    let mut first = String::new();
    r.read_line(&mut first).map_err(|e| format!("the printer's storage stopped answering ({e})"))?;
    let code: u32 = first.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| format!("odd answer from the printer's storage: {}", first.trim()))?;
    let mut msg = first.clone();
    if first.as_bytes().get(3) == Some(&b'-') {
        // multi-line reply: until "<code> "
        for _ in 0..200 {
            let mut l = String::new();
            if r.read_line(&mut l).map_err(|e| e.to_string())? == 0 {
                break;
            }
            msg.push_str(&l);
            if l.starts_with(&format!("{code} ")) {
                break;
            }
        }
    }
    Ok((code, msg.trim().to_string()))
}

fn ftp_cmd<S: Read + Write>(s: &mut BufReader<S>, cmd: &str) -> Result<(u32, String)> {
    s.get_mut().write_all(format!("{cmd}\r\n").as_bytes()).map_err(|e| format!("couldn't talk to the printer's storage ({e})"))?;
    s.get_mut().flush().ok();
    ftp_reply(s)
}

fn pasv_port(msg: &str) -> Option<u16> {
    let inner = msg.split('(').nth(1)?.split(')').next()?;
    let n: Vec<u16> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if n.len() != 6 {
        return None;
    }
    n[4].checked_mul(256)?.checked_add(n[5])
}

fn ftps_store(cfg: &Arc<ClientConfig>, host: &str, code: &str, name: &str, data: &[u8], progress: &mut dyn FnMut(u64, u64)) -> Result<()> {
    let to = Duration::from_secs(20);
    let sock = tcp(host, FTP_PORT, to).map_err(|e| format!("{e}. Is the printer's storage (SD card) in, and LAN Only mode on?"))?;
    let mut c = BufReader::new(tls(cfg, host, sock)?);
    let (g, msg) = ftp_reply(&mut c)?;
    if g != 220 {
        return Err(format!("the printer's storage said: {msg}"));
    }
    if ftp_cmd(&mut c, &format!("USER {USER}"))?.0 != 331 {
        return Err("the printer's storage didn't take the login".into());
    }
    let (p, pm) = ftp_cmd(&mut c, &format!("PASS {}", code.trim()))?;
    if p != 230 {
        return Err(format!("the printer refused the file login ({pm})"));
    }
    ftp_cmd(&mut c, "PBSZ 0")?;
    ftp_cmd(&mut c, "PROT P")?;
    ftp_cmd(&mut c, "TYPE I")?;
    let (pc, pasv) = ftp_cmd(&mut c, "PASV")?;
    let port = if pc == 227 { pasv_port(&pasv) } else { None }.ok_or_else(|| format!("the printer's storage didn't open a transfer ({pasv})"))?;
    let dsock = tcp(host, port, to)?;
    let safe: String = name.chars().filter(|ch| !matches!(ch, '\r' | '\n' | '/' | '\\')).collect();
    c.get_mut().write_all(format!("STOR {safe}\r\n").as_bytes()).map_err(|e| e.to_string())?;
    // the data connection must resume the control connection's TLS session (same config)
    let mut d = tls(cfg, host, dsock)?;
    let (sc, sm) = ftp_reply(&mut c)?;
    if sc != 150 && sc != 125 {
        return Err(format!("the printer's storage won't take the file ({sm})"));
    }
    let total = data.len() as u64;
    let mut sent = 0u64;
    for chunk in data.chunks(64 * 1024) {
        d.write_all(chunk).map_err(|e| format!("sending the file to the printer failed ({e})"))?;
        sent += chunk.len() as u64;
        progress(sent, total);
    }
    d.flush().ok();
    // no TLS close on the data connection: the printer never answers it; just close the socket
    d.sock.shutdown(std::net::Shutdown::Both).ok();
    drop(d);
    let (dc, dm) = ftp_reply(&mut c)?;
    if dc != 226 && dc != 250 {
        return Err(format!("the printer didn't keep the file ({dm})"));
    }
    ftp_cmd(&mut c, "QUIT").ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tray(ams: u32, slot: u32) -> Tray {
        Tray { ams, slot, material: "PLA".into(), ..Default::default() }
    }

    #[test]
    fn print_command_like_bambu_studio() {
        let opts = PrintOptions { bed_leveling: "auto".into(), flow_cali: "off".into(), ..Default::default() };
        // A1: Auto -> On, mapping padded to 5, no ams_mapping2
        let p = print_payload("x.gcode.3mf", 1, "N2S", &[Some(tray(0, 1)), None], &opts, 7, "x");
        let p = &p["print"];
        assert_eq!(p["ams_mapping"], json!([1, -1, -1, -1, -1]));
        assert_eq!(p["auto_bed_leveling"], json!(1));
        assert_eq!(p["extrude_cali_flag"], json!(0));
        assert_eq!(p["url"], json!("ftp:///x.gcode.3mf"));
        assert_eq!(p["param"], json!("Metadata/plate_1.gcode"));
        assert!(p.get("nozzle_offset_cali").is_none());
        // H2S: Auto stays, ams_mapping2 by unit and slot
        let p = print_payload("x.gcode.3mf", 1, "O1S", &[Some(tray(1, 2))], &opts, 1, "x");
        assert_eq!(p["print"]["auto_bed_leveling"], json!(2));
        assert_eq!(p["print"]["ams_mapping"], json!([6]));
        assert_eq!(p["print"]["ams_mapping2"], json!([{"ams_id": 1, "slot_id": 2}]));
        // the external spool only: no AMS
        let p = print_payload("x.gcode.3mf", 1, "N2S", &[Some(tray(255, 0))], &opts, 1, "x");
        assert_eq!(p["print"]["use_ams"], json!(false));
        // H2D gets nozzle offset calibration
        assert!(print_payload("x", 1, "O1D", &[], &opts, 1, "x")["print"].get("nozzle_offset_cali").is_some());
    }

    #[test]
    fn status_and_replies() {
        let rep: Value = json!({"gcode_state": "RUNNING", "mc_percent": 42, "mc_remaining_time": 75, "layer_num": 10, "total_layer_num": 100,
            "ams": {"ams": [{"id": "0", "tray": [{"id": "0", "tray_type": "PLA", "tray_color": "FFFFFFFF", "tray_info_idx": "P1234567"}, {"id": "1"}]}]},
            "vt_tray": {"id": "254", "tray_type": "PETG", "tray_color": "00AE42FF"}});
        let s = parse_status(rep.as_object().unwrap_or(&Map::new()));
        assert_eq!(s.summary(), "Printing 42% · layer 10/100 · 1h 15m left");
        assert_eq!(s.trays.len(), 3);
        assert_eq!((s.trays[0].filament_id.as_str(), s.trays[0].color.as_str()), ("P1234567", "#FFFFFF"));
        assert_eq!(s.trays[1].label(), "AMS 1 slot 2: empty");
        assert_eq!((s.trays[2].index(), s.trays[2].label()), (254, "External spool: PETG".to_string()));
        let bad = json!({"command": "project_file", "result": "fail", "err_code": NEED_DEV_MODE});
        assert!(check_reply(bad.as_object().unwrap_or(&Map::new())).is_err_and(|e| e.contains("Developer Mode")));
        assert!(check_reply(json!({"command": "pause", "result": "success"}).as_object().unwrap_or(&Map::new())).is_ok());
        // hostile report values don't panic
        let junk = json!({"mc_percent": "lots", "ams": {"ams": [{"id": "x"}, 5, {"id": "1", "tray": "no"}]}, "vt_tray": 7});
        let _ = parse_status(junk.as_object().unwrap_or(&Map::new()));
    }

    #[test]
    fn mqtt_and_ftp_bits() {
        let mut v = Vec::new();
        enc_len(321, &mut v);
        assert_eq!(v, vec![0xC1, 0x02]);
        let mut c = std::io::Cursor::new(packet(0x30, &{
            let mut b = Vec::new();
            mstr("device/1/report", &mut b);
            b.extend_from_slice(b"{}");
            b
        }));
        let (h, body) = read_packet(&mut c).unwrap_or_default();
        assert_eq!(publish_parts(h, &body).map(|(t, p)| (t, p.to_vec())), Some(("device/1/report".into(), b"{}".to_vec())));
        let ann = "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1990\r\nLocation: 192.168.1.20\r\nNT: urn:bambulab-com:device:3dprinter:1\r\nUSN: 03900A000000001\r\nDevModel.bambu.com: N2S\r\nDevName.bambu.com: My A1\r\n\r\n";
        assert_eq!(
            parse_ssdp(ann, "10.0.0.9"),
            Some(Found { ip: "192.168.1.20".into(), serial: "03900A000000001".into(), model: "N2S".into(), name: "My A1".into() })
        );
        assert_eq!(parse_ssdp("HTTP/1.1 200 OK\r\nUSN: x\r\n", "1.2.3.4"), None);
        assert_eq!(pasv_port("227 Entering Passive Mode (192,168,1,20,4,1)."), Some(1025));
        assert_eq!(pasv_port("227 nonsense"), None);
        assert_eq!(publish_parts(0x30, &[0x00]), None);
        let mut r = std::io::Cursor::new(b"230-Welcome\r\n more\r\n230 OK\r\n".to_vec());
        assert_eq!(ftp_reply(&mut r).map(|(c, _)| c), Ok(230));
    }
}
