//! Viewing a question's attached files inside the exam. A locked exam can't hand files to other
//! programs, so everything opens here: images, text and code, packet captures (.pcap/.pcapng,
//! Wireshark-style list + hex dump) and, for anything else, a hex view.

use crate::ui::theme::{self, pal};
use egui::{Align, Layout, RichText};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FileKind {
    Image,
    Text,
    Capture,
    Binary,
}

pub fn kind_of(name: &str, data: &[u8]) -> FileKind {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => FileKind::Image,
        "pcap" | "pcapng" | "cap" => FileKind::Capture,
        _ if pcap::parse(data).is_some() => FileKind::Capture,
        _ if looks_like_text(data) => FileKind::Text,
        _ => FileKind::Binary,
    }
}

fn looks_like_text(data: &[u8]) -> bool {
    let head = &data[..data.len().min(8192)];
    !head.contains(&0) && std::str::from_utf8(head).is_ok() || (!head.contains(&0) && head.iter().filter(|b| b.is_ascii_graphic() || b.is_ascii_whitespace()).count() * 10 >= head.len() * 9)
}

pub fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{:.0} KB", (bytes as f32 / 1024.0).max(1.0)) } else { format!("{:.1} MB", bytes as f32 / (1024.0 * 1024.0)) }
}

pub fn kind_label(k: FileKind) -> &'static str {
    match k {
        FileKind::Image => "Image",
        FileKind::Text => "Text",
        FileKind::Capture => "Packet capture",
        FileKind::Binary => "File",
    }
}

/// State of the open viewer (filter, selected packet).
#[derive(Default)]
pub struct ViewerState {
    pub filter: String,
    pub selected: Option<usize>,
}

/// Draws `data` (named `name`) filling `ui`.
pub fn show(ui: &mut egui::Ui, id: &str, name: &str, data: &[u8], st: &mut ViewerState) {
    match kind_of(name, data) {
        FileKind::Image => {
            let uri = format!("bytes://attachment/{id}/{name}");
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::from_bytes(uri, data.to_vec()).fit_to_exact_size(ui.available_size()).maintain_aspect_ratio(true));
                });
            });
        }
        FileKind::Text => text_view(ui, data),
        FileKind::Capture => match pcap::parse(data) {
            Some(packets) => capture_view(ui, &packets, st),
            None => hex_view(ui, data, 0),
        },
        FileKind::Binary => hex_view(ui, data, 0),
    }
}

/// Rows exactly `row_h` tall (the app's controls are 32 px tall by default).
fn dense(ui: &mut egui::Ui, row_h: f32) {
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().interact_size.y = row_h;
}

fn text_view(ui: &mut egui::Ui, data: &[u8]) {
    let p = pal();
    let text = String::from_utf8_lossy(data);
    let lines: Vec<&str> = text.lines().collect();
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, lines.len().max(1), |ui, range| {
        dense(ui, row_h);
        for i in range {
            ui.horizontal(|ui| {
                ui.add_sized([48.0, row_h], egui::Label::new(RichText::new(format!("{}", i + 1)).font(theme::mono(12.5)).color(p.faint)));
                ui.label(RichText::new(lines.get(i).copied().unwrap_or("")).font(theme::mono(13.0)).color(p.text));
            });
        }
    });
}

/// Offset | 16 bytes in hex | ASCII.
fn hex_view(ui: &mut egui::Ui, data: &[u8], base: usize) {
    let p = pal();
    let rows = data.len().div_ceil(16).max(1);
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    egui::ScrollArea::both().id_salt(("hex", base, data.len())).auto_shrink([false, false]).show_rows(ui, row_h, rows, |ui, range| {
        dense(ui, row_h);
        for r in range {
            let chunk = &data[r * 16..((r + 1) * 16).min(data.len())];
            let hex: String = (0..16).map(|i| chunk.get(i).map(|b| format!("{b:02x} ")).unwrap_or_else(|| "   ".into()) + if i == 7 { " " } else { "" }).collect();
            let ascii: String = chunk.iter().map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' }).collect();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{:08x}", base + r * 16)).font(theme::mono(12.5)).color(p.faint));
                ui.label(RichText::new(hex).font(theme::mono(12.5)).color(p.text));
                ui.label(RichText::new(ascii).font(theme::mono(12.5)).color(p.accent));
            });
        }
    });
}

fn capture_view(ui: &mut egui::Ui, packets: &[pcap::Packet], st: &mut ViewerState) {
    let p = pal();
    ui.horizontal(|ui| {
        ui.label(theme::eyebrow("Filter"));
        ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("ip, port, protocol or text — e.g. 10.0.0.5, dns, GET").desired_width(360.0).margin(egui::Margin::symmetric(8, 5)));
        ui.label(theme::muted(format!("{} packets", packets.len())).size(12.5));
    });
    let f = st.filter.trim().to_lowercase();
    let shown: Vec<usize> = (0..packets.len()).filter(|i| f.is_empty() || packets[*i].matches(&f)).collect();
    let list_h = (ui.available_height() * 0.55).max(120.0);
    let cols = [52.0, 82.0, 150.0, 150.0, 70.0, 56.0];
    let header = ["No.", "Time", "Source", "Destination", "Protocol", "Length", "Info"];
    // Headers line up with the left-aligned values below.
    let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let mut hx = hr.left() + 4.0;
    for (i, h) in header.iter().enumerate() {
        ui.painter().text(egui::pos2(hx, hr.center().y), egui::Align2::LEFT_CENTER, h.to_uppercase(), theme::mono(11.0), p.muted);
        hx += cols.get(i).copied().unwrap_or(0.0);
    }
    let row_h = 20.0;
    egui::ScrollArea::vertical().id_salt("packets").max_height(list_h).auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
        for r in range {
            let i = shown[r];
            let pk = &packets[i];
            let sel = st.selected == Some(i);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
            let fill = if sel { p.accent_soft } else if resp.hovered() { p.hover } else if r % 2 == 1 { p.surface2 } else { egui::Color32::TRANSPARENT };
            ui.painter().rect_filled(rect, 0.0, fill);
            let mut x = rect.left() + 4.0;
            let texts = [format!("{}", i + 1), format!("{:.6}", pk.time), pk.src.clone(), pk.dst.clone(), pk.proto.clone(), format!("{}", pk.data.len()), pk.info.clone()];
            for (c, t) in texts.iter().enumerate() {
                let w = cols.get(c).copied().unwrap_or(rect.right() - x);
                let painter = ui.painter().with_clip_rect(egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2((x + w - 6.0).min(rect.right()), rect.bottom())));
                painter.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, t, theme::mono(12.5), if c == 4 { p.info } else { p.text });
                x += w;
            }
            if resp.clicked() {
                st.selected = Some(i);
            }
        }
    });
    ui.separator();
    match st.selected.and_then(|i| packets.get(i)) {
        Some(pk) => {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                for line in &pk.details {
                    ui.label(RichText::new(line).font(theme::mono(12.5)).color(p.text));
                }
                ui.add_space(4.0);
                hex_view(ui, &pk.data, 0);
            });
        }
        None => {
            ui.label(theme::muted("Select a packet to see its headers and bytes."));
        }
    }
}

/// Minimal pcap / pcapng reader and Ethernet/IP/TCP/UDP decoder.
pub mod pcap {
    pub struct Packet {
        pub time: f64,
        pub src: String,
        pub dst: String,
        pub proto: String,
        pub info: String,
        pub details: Vec<String>,
        pub data: Vec<u8>,
    }

    impl Packet {
        pub fn matches(&self, f: &str) -> bool {
            [&self.src, &self.dst, &self.proto, &self.info].iter().any(|s| s.to_lowercase().contains(f)) || self.details.iter().any(|d| d.to_lowercase().contains(f))
        }
    }

    const MAX_PACKETS: usize = 50_000;

    /// Raw frames with (time in seconds, link type, bytes).
    fn frames(d: &[u8]) -> Option<Vec<(f64, u32, Vec<u8>)>> {
        if d.len() < 24 {
            return None;
        }
        let m = u32::from_le_bytes(d[0..4].try_into().ok()?);
        let (le, nanos) = match m {
            0xa1b2c3d4 => (true, false),
            0xa1b23c4d => (true, true),
            0xd4c3b2a1 => (false, false),
            0x4d3cb2a1 => (false, true),
            0x0a0d0d0a => return pcapng(d),
            _ => return None,
        };
        let u32_at = |o: usize| -> Option<u32> {
            let b: [u8; 4] = d.get(o..o + 4)?.try_into().ok()?;
            Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
        };
        let link = u32_at(20)?;
        let mut out = Vec::new();
        let mut o = 24;
        while o + 16 <= d.len() && out.len() < MAX_PACKETS {
            let (sec, frac, incl) = (u32_at(o)?, u32_at(o + 4)?, u32_at(o + 8)? as usize);
            let data = d.get(o + 16..o + 16 + incl)?.to_vec();
            out.push((sec as f64 + frac as f64 / if nanos { 1e9 } else { 1e6 }, link, data));
            o += 16 + incl;
        }
        Some(out)
    }

    fn pcapng(d: &[u8]) -> Option<Vec<(f64, u32, Vec<u8>)>> {
        let bom = u32::from_le_bytes(d.get(8..12)?.try_into().ok()?);
        let le = bom == 0x1a2b3c4d;
        if !le && bom != 0x4d3c2b1a {
            return None;
        }
        let u32_at = |o: usize| -> Option<u32> {
            let b: [u8; 4] = d.get(o..o + 4)?.try_into().ok()?;
            Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
        };
        let u16_at = |o: usize| -> Option<u16> {
            let b: [u8; 2] = d.get(o..o + 2)?.try_into().ok()?;
            Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
        };
        let mut ifaces: Vec<(u32, f64)> = Vec::new(); // (link type, ticks per second)
        let mut out = Vec::new();
        let mut o = 0;
        while o + 12 <= d.len() && out.len() < MAX_PACKETS {
            let (kind, len) = (u32_at(o)?, u32_at(o + 4)? as usize);
            if len < 12 || o + len > d.len() {
                break;
            }
            match kind {
                0x0a0d0d0a => ifaces.clear(),
                1 => {
                    let link = u16_at(o + 8)? as u32;
                    let mut res = 1e6;
                    let mut p = o + 16;
                    while p + 4 <= o + len - 4 {
                        let (code, olen) = (u16_at(p)?, u16_at(p + 2)? as usize);
                        if code == 0 {
                            break;
                        }
                        if code == 9 && olen >= 1 {
                            let v = *d.get(p + 4)?;
                            res = if v & 0x80 != 0 { 2f64.powi((v & 0x7f) as i32) } else { 10f64.powi(v as i32) };
                        }
                        p += 4 + olen.div_ceil(4) * 4;
                    }
                    ifaces.push((link, res));
                }
                6 => {
                    let iface = u32_at(o + 8)? as usize;
                    let ts = ((u32_at(o + 12)? as u64) << 32) | u32_at(o + 16)? as u64;
                    let cap = u32_at(o + 20)? as usize;
                    let (link, res) = ifaces.get(iface).copied().unwrap_or((1, 1e6));
                    out.push((ts as f64 / res, link, d.get(o + 28..o + 28 + cap)?.to_vec()));
                }
                3 => {
                    let orig = u32_at(o + 8)? as usize;
                    let cap = orig.min(len - 16);
                    let link = ifaces.first().map(|i| i.0).unwrap_or(1);
                    out.push((0.0, link, d.get(o + 12..o + 12 + cap)?.to_vec()));
                }
                _ => {}
            }
            o += len;
        }
        Some(out)
    }

    pub fn parse(d: &[u8]) -> Option<Vec<Packet>> {
        let frames = frames(d)?;
        let t0 = frames.first().map(|f| f.0).unwrap_or(0.0);
        Some(frames.into_iter().map(|(t, link, data)| decode(t - t0, link, data)).collect())
    }

    fn mac(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
    }

    fn be16(b: &[u8], o: usize) -> u16 {
        b.get(o..o + 2).map(|x| u16::from_be_bytes([x[0], x[1]])).unwrap_or(0)
    }

    fn port_name(p: u16) -> Option<&'static str> {
        Some(match p {
            20 | 21 => "FTP",
            22 => "SSH",
            23 => "Telnet",
            25 | 587 => "SMTP",
            53 => "DNS",
            67 | 68 => "DHCP",
            80 | 8080 | 8000 => "HTTP",
            110 => "POP3",
            123 => "NTP",
            143 => "IMAP",
            161 => "SNMP",
            443 | 8443 => "TLS",
            445 => "SMB",
            3306 => "MySQL",
            3389 => "RDP",
            _ => return None,
        })
    }

    fn printable_line(b: &[u8]) -> Option<String> {
        let line: Vec<u8> = b.iter().take_while(|c| **c != b'\r' && **c != b'\n').take(120).copied().collect();
        (line.len() >= 3 && line.iter().all(|c| c.is_ascii_graphic() || *c == b' ')).then(|| String::from_utf8_lossy(&line).into_owned())
    }

    fn dns_name(b: &[u8], mut o: usize) -> Option<String> {
        let mut parts = Vec::new();
        let mut jumps = 0;
        loop {
            let len = *b.get(o)? as usize;
            if len == 0 {
                break;
            }
            if len & 0xc0 == 0xc0 {
                o = ((len & 0x3f) << 8) | *b.get(o + 1)? as usize;
                jumps += 1;
                if jumps > 10 {
                    return None;
                }
                continue;
            }
            parts.push(String::from_utf8_lossy(b.get(o + 1..o + 1 + len)?).into_owned());
            o += 1 + len;
        }
        Some(parts.join("."))
    }

    fn decode(time: f64, link: u32, data: Vec<u8>) -> Packet {
        let mut pk = Packet { time, src: String::new(), dst: String::new(), proto: "Frame".into(), info: String::new(), details: vec![format!("Frame: {} bytes", data.len())], data };
        let d = pk.data.clone();
        // Find the network layer.
        let (mut ethertype, mut off) = match link {
            1 => {
                if d.len() < 14 {
                    return pk;
                }
                pk.details.push(format!("Ethernet: {} → {}", mac(&d[6..12]), mac(&d[0..6])));
                pk.src = mac(&d[6..12]);
                pk.dst = mac(&d[0..6]);
                (be16(&d, 12), 14)
            }
            101 | 12 | 14 => (if d.first().is_some_and(|b| b >> 4 == 6) { 0x86dd } else { 0x0800 }, 0),
            113 => (be16(&d, 14), 16),
            276 => (be16(&d, 0), 20),
            _ => return pk,
        };
        while ethertype == 0x8100 && d.len() >= off + 4 {
            ethertype = be16(&d, off + 2);
            off += 4;
        }
        let (proto_num, l4, src_ip, dst_ip) = match ethertype {
            0x0800 if d.len() >= off + 20 => {
                let ihl = ((d[off] & 0x0f) as usize) * 4;
                let s = format!("{}.{}.{}.{}", d[off + 12], d[off + 13], d[off + 14], d[off + 15]);
                let t = format!("{}.{}.{}.{}", d[off + 16], d[off + 17], d[off + 18], d[off + 19]);
                pk.details.push(format!("IPv4: {s} → {t}, TTL {}, protocol {}", d[off + 8], d[off + 9]));
                (d[off + 9], off + ihl, s, t)
            }
            0x86dd if d.len() >= off + 40 => {
                let fmt = |b: &[u8]| b.chunks(2).map(|c| format!("{:x}", u16::from_be_bytes([c[0], c[1]]))).collect::<Vec<_>>().join(":");
                let s = fmt(&d[off + 8..off + 24]);
                let t = fmt(&d[off + 24..off + 40]);
                pk.details.push(format!("IPv6: {s} → {t}, next header {}", d[off + 6]));
                (d[off + 6], off + 40, s, t)
            }
            0x0806 if d.len() >= off + 28 => {
                pk.proto = "ARP".into();
                let sip = format!("{}.{}.{}.{}", d[off + 14], d[off + 15], d[off + 16], d[off + 17]);
                let tip = format!("{}.{}.{}.{}", d[off + 24], d[off + 25], d[off + 26], d[off + 27]);
                pk.info = if be16(&d, off + 6) == 1 { format!("Who has {tip}? Tell {sip}") } else { format!("{sip} is at {}", mac(&d[off + 8..off + 14])) };
                pk.details.push(format!("ARP: {}", pk.info));
                return pk;
            }
            _ => return pk,
        };
        pk.src = src_ip;
        pk.dst = dst_ip;
        match proto_num {
            6 if d.len() >= l4 + 20 => {
                let (sp, dp) = (be16(&d, l4), be16(&d, l4 + 2));
                let hl = ((d[l4 + 12] >> 4) as usize) * 4;
                let fl = d[l4 + 13];
                let flags: Vec<&str> = [(0x02, "SYN"), (0x10, "ACK"), (0x01, "FIN"), (0x04, "RST"), (0x08, "PSH"), (0x20, "URG")].iter().filter(|(b, _)| fl & b != 0).map(|(_, n)| *n).collect();
                let payload = d.get(l4 + hl..).unwrap_or(&[]);
                pk.proto = "TCP".into();
                pk.info = format!("{sp} → {dp} [{}] Len={}", flags.join(", "), payload.len());
                pk.details.push(format!("TCP: port {sp} → {dp}, seq {}, ack {}, flags [{}]", u32::from_be_bytes(d[l4 + 4..l4 + 8].try_into().unwrap()), u32::from_be_bytes(d[l4 + 8..l4 + 12].try_into().unwrap()), flags.join(", ")));
                if !payload.is_empty() {
                    if let Some(name) = port_name(sp).or(port_name(dp)) {
                        pk.proto = name.into();
                    }
                    if let Some(line) = printable_line(payload) {
                        pk.info = line;
                    }
                    pk.details.push(format!("Payload: {} bytes", payload.len()));
                }
            }
            17 if d.len() >= l4 + 8 => {
                let (sp, dp) = (be16(&d, l4), be16(&d, l4 + 2));
                let payload = d.get(l4 + 8..).unwrap_or(&[]);
                pk.proto = port_name(sp).or(port_name(dp)).unwrap_or("UDP").into();
                pk.info = format!("{sp} → {dp} Len={}", payload.len());
                pk.details.push(format!("UDP: port {sp} → {dp}, {} bytes", payload.len()));
                if (sp == 53 || dp == 53) && payload.len() > 12 {
                    let qr = payload[2] & 0x80 != 0;
                    if let Some(name) = dns_name(payload, 12) {
                        pk.info = format!("{} {name}", if qr { "Standard query response" } else { "Standard query" });
                        pk.details.push(format!("DNS: {name}"));
                    }
                } else if let Some(line) = printable_line(payload) {
                    pk.info = line;
                }
            }
            1 | 58 if d.len() > l4 + 1 => {
                pk.proto = if proto_num == 1 { "ICMP".into() } else { "ICMPv6".into() };
                let t = d[l4];
                pk.info = match (proto_num, t) {
                    (1, 8) | (58, 128) => "Echo (ping) request".into(),
                    (1, 0) | (58, 129) => "Echo (ping) reply".into(),
                    _ => format!("Type {t}"),
                };
            }
            n => pk.proto = format!("IP proto {n}"),
        }
        pk
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Classic pcap with one Ethernet/IPv4/TCP packet carrying an HTTP request.
        fn sample() -> Vec<u8> {
            let payload = b"GET /flag.txt HTTP/1.1\r\nHost: lab\r\n\r\n";
            let mut tcp = vec![0x04, 0xd2, 0x00, 0x50, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x18, 0xff, 0xff, 0, 0, 0, 0];
            tcp.extend_from_slice(payload);
            let total = 20 + tcp.len();
            let mut ip = vec![0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0, 0, 64, 6, 0, 0, 10, 0, 0, 5, 10, 0, 0, 9];
            ip.extend(tcp);
            let mut eth = vec![0xaa; 6];
            eth.extend([0xbb; 6]);
            eth.extend([0x08, 0x00]);
            eth.extend(ip);
            let mut f = Vec::new();
            for v in [0xa1b2c3d4u32] {
                f.extend(v.to_le_bytes());
            }
            f.extend(2u16.to_le_bytes());
            f.extend(4u16.to_le_bytes());
            f.extend([0u8; 8]);
            f.extend(65535u32.to_le_bytes());
            f.extend(1u32.to_le_bytes());
            f.extend(10u32.to_le_bytes());
            f.extend(500u32.to_le_bytes());
            f.extend((eth.len() as u32).to_le_bytes());
            f.extend((eth.len() as u32).to_le_bytes());
            f.extend(eth);
            f
        }

        #[test]
        fn decodes_http_over_tcp() {
            let pk = parse(&sample()).expect("pcap");
            assert_eq!(pk.len(), 1);
            assert_eq!(pk[0].src, "10.0.0.5");
            assert_eq!(pk[0].dst, "10.0.0.9");
            assert_eq!(pk[0].proto, "HTTP");
            assert_eq!(pk[0].info, "GET /flag.txt HTTP/1.1");
            assert!(pk[0].matches("flag"));
            assert!(parse(b"not a capture at all, just text").is_none());
        }
    }
}
