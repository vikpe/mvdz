//! Structure-aware MVD generator: mostly well-formed for the protocol state in force, so
//! the walker reaches its deep paths, with occasional garbage.
#![allow(dead_code)]

const PROTOCOL_VERSION_FTE: u32 = u32::from_le_bytes(*b"FTEX");
const PROTOCOL_VERSION_MVD_1: u32 = u32::from_le_bytes(*b"MVD1");
const FTE_PEXT_FLOAT_COORDS: u32 = 1 << 15;
const MVD_PEXT_1_FLOAT_COORDS: u32 = 1 << 0;

/// Tracked from the serverdata the generator writes, independently of the library.
#[derive(Debug, Default)]
pub struct ProtocolState {
    pub fte: u32,
    mvd1: u32,
}

impl ProtocolState {
    fn has_fte_float_coords(&self) -> bool {
        self.fte & FTE_PEXT_FLOAT_COORDS != 0
    }
    pub fn coord_size(&self) -> usize {
        if self.has_fte_float_coords() || self.mvd1 & MVD_PEXT_1_FLOAT_COORDS != 0 {
            4
        } else {
            2
        }
    }
    pub fn wide_coord_size(&self) -> usize {
        if self.has_fte_float_coords() { 4 } else { 2 }
    }
    pub fn angle_size(&self) -> usize {
        if self.has_fte_float_coords() { 2 } else { 1 }
    }
}

pub struct Chooser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Chooser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Chooser { bytes, position: 0 }
    }
    pub fn is_done(&self) -> bool {
        self.position >= self.bytes.len()
    }
    pub fn u8(&mut self) -> u8 {
        let value = self.bytes.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        value
    }
    pub fn u16(&mut self) -> u16 {
        self.u8() as u16 | (self.u8() as u16) << 8
    }
    pub fn u32(&mut self) -> u32 {
        self.u16() as u32 | (self.u16() as u32) << 16
    }
    pub fn below(&mut self, bound: usize) -> usize {
        self.u8() as usize % bound.max(1)
    }
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.u8()).collect()
    }
    pub fn text(&mut self, len: usize) -> Vec<u8> {
        const ALPHABET: &[u8] = b"abc \\\";\n/xyz0129matchdate:epoch";
        (0..len)
            .map(|_| ALPHABET[self.below(ALPHABET.len())])
            .collect()
    }
}

fn push_string(out: &mut Vec<u8>, text: &[u8]) {
    out.extend(text.iter().map(|&byte| if byte == 0 { b'.' } else { byte }));
    out.push(0);
}

fn delta(chooser: &mut Chooser, protocol: &ProtocolState, out: &mut Vec<u8>, entity: u16) {
    let high = chooser.u8() as u16 & 0x7f;
    let mut header = (high << 9) | (entity & 511);
    if header == 0 {
        header = 1 << 9;
    }
    out.extend_from_slice(&header.to_le_bytes());
    let mut bits = (header as u32) & !511;
    let mut more = 0u32;
    if bits & (1 << 15) != 0 {
        let more_bits = chooser.u8();
        bits |= more_bits as u32;
        out.push(more_bits);
    }
    if bits & (1 << 7) != 0 && protocol.fte != 0 {
        let fte_bits = chooser.u8();
        more = fte_bits as u32;
        out.push(fte_bits);
        if fte_bits & 0x80 != 0 {
            let fte_high_bits = chooser.u8() & 0x07;
            more |= (fte_high_bits as u32) << 8;
            out.push(fte_high_bits);
        }
    }
    if bits & (1 << 2) != 0 {
        out.push(chooser.u8());
    } else if more & (1 << 3) != 0 {
        out.extend(chooser.bytes(2));
    }
    for bit in [1 << 13, 1 << 3, 1 << 4, 1 << 5] {
        if bits & bit != 0 {
            out.push(chooser.u8());
        }
    }
    for (origin_bit, angle_bit) in [(1 << 9, 1), (1 << 10, 1 << 12), (1 << 11, 1 << 1)] {
        if bits & origin_bit != 0 {
            out.extend(chooser.bytes(protocol.coord_size()));
        }
        if bits & angle_bit != 0 {
            out.extend(chooser.bytes(protocol.angle_size()));
        }
    }
    if more & 2 != 0 && protocol.fte & 8 != 0 {
        out.push(chooser.u8());
    }
    if more & 1 != 0 {
        out.push(chooser.u8());
    }
    if more & 4 != 0 {
        out.push(chooser.u8());
    }
    if more & (1 << 10) != 0 && protocol.fte & 0x80000 != 0 {
        out.extend(chooser.bytes(3));
    }
}

fn command(chooser: &mut Chooser, protocol: &mut ProtocolState, out: &mut Vec<u8>) {
    let kind = chooser.below(40);
    let coord_len = protocol.coord_size();
    match kind {
        0..=6 => {
            // playerinfo
            out.push(42);
            out.push(chooser.below(34) as u8);
            let flags = chooser.u16() & 0x1fff | if chooser.below(8) == 0 { 0x8000 } else { 0 };
            out.extend_from_slice(&flags.to_le_bytes());
            out.push(chooser.u8());
            for axis in 0..3 {
                if flags & (1 << axis) != 0 {
                    out.extend(chooser.bytes(coord_len));
                }
            }
            for axis in 0..3 {
                if flags & (8 << axis) != 0 {
                    out.extend(chooser.bytes(2));
                }
            }
            for bit in [1u16 << 11, 1 << 7, 1 << 6, 1 << 10] {
                if flags & bit != 0 {
                    out.push(chooser.u8());
                }
            }
        }
        7..=11 => {
            // packet entities
            let is_delta_packet = chooser.below(2) == 1;
            out.push(if is_delta_packet { 48 } else { 47 });
            if is_delta_packet {
                out.push(chooser.u8());
            }
            let count = chooser.below(12);
            let mut entity = chooser.below(40) as u16;
            for _ in 0..count {
                delta(chooser, protocol, out, entity);
                let gap_bound = if chooser.below(16) == 0 { 400 } else { 4 };
                entity = entity.wrapping_add(1 + chooser.below(gap_bound) as u16);
            }
            out.extend_from_slice(&[0, 0]);
        }
        12..=14 => {
            out.push(6);
            let channel = chooser.u16();
            out.extend_from_slice(&channel.to_le_bytes());
            if channel & 0x8000 != 0 {
                out.push(chooser.u8());
            }
            if channel & 0x4000 != 0 {
                out.push(chooser.u8());
            }
            out.push(chooser.u8());
            out.extend(chooser.bytes(3 * coord_len));
        }
        15..=17 => {
            out.push(23);
            let temp_entity = chooser.below(15) as u8;
            out.push(temp_entity);
            match temp_entity {
                5 | 6 | 9 => {
                    let entity = if chooser.below(2) == 0 {
                        1 + chooser.below(32) as u16
                    } else {
                        chooser.u16()
                    };
                    out.extend_from_slice(&entity.to_le_bytes());
                    out.extend(chooser.bytes(6 * coord_len));
                }
                2 | 12 => {
                    out.push(chooser.u8());
                    out.extend(chooser.bytes(3 * coord_len));
                }
                14 => out.extend(chooser.bytes(3 * coord_len)), // unknown TE: escape
                _ => out.extend(chooser.bytes(3 * coord_len)),
            }
        }
        18 => {
            out.push(3);
            out.extend(chooser.bytes(2));
        }
        19 => {
            out.push(38);
            out.extend(chooser.bytes(5));
        }
        20 => {
            out.push(14);
            out.extend(chooser.bytes(3));
        }
        21 => {
            out.push(8);
            out.push(chooser.below(5) as u8);
            let len = chooser.below(40);
            let mut text = if chooser.below(4) == 0 {
                b"matchdate: 2024-01-02 03:04:05 CET\n".to_vec()
            } else {
                Vec::new()
            };
            text.extend(chooser.text(len));
            push_string(out, &text);
        }
        22 => {
            out.push(9);
            let len = chooser.below(60);
            let mut text = if chooser.below(3) == 0 {
                b"fullserverinfo \"\\epoch\\1700000000\\map\\dm4\"\n".to_vec()
            } else {
                Vec::new()
            };
            text.extend(chooser.text(len));
            push_string(out, &text);
        }
        23 => {
            let command_byte = [36u8, 37, 53][chooser.below(3)];
            out.push(command_byte);
            out.extend(chooser.bytes(match command_byte {
                36 => 3,
                37 => 5,
                _ => 2,
            }));
        }
        24 => {
            let command_byte = if chooser.below(2) == 0 { 43 } else { 54 };
            out.push(command_byte);
            let count = chooser.below(6);
            out.push(count as u8);
            out.extend(chooser.bytes(count * if command_byte == 43 { 6 } else { 7 }));
        }
        25 => {
            // serverdata with protocol extensions
            out.push(11);
            let mut server_data = Vec::new();
            for _ in 0..chooser.below(4) {
                let (tag, value) = match chooser.below(4) {
                    0 => (
                        0x5845_5446u32,
                        [
                            0u32,
                            0x8000,
                            0x8 | 0x8000 | 0x80000,
                            0x8 | 0x80000,
                            chooser.u32(),
                        ][chooser.below(5)],
                    ),
                    1 => (0x3245_5446, chooser.u32()),
                    2 => (0x3144_564D, [0u32, 1, chooser.u32()][chooser.below(3)]),
                    _ => (chooser.u32() | 0x100, chooser.u32()),
                };
                server_data.extend_from_slice(&tag.to_le_bytes());
                server_data.extend_from_slice(&value.to_le_bytes());
                match tag {
                    PROTOCOL_VERSION_FTE => protocol.fte = value,
                    PROTOCOL_VERSION_MVD_1 => protocol.mvd1 = value,
                    _ => {}
                }
            }
            server_data.extend_from_slice(&28u32.to_le_bytes());
            server_data.extend(chooser.bytes(4));
            push_string(&mut server_data, b"qw");
            server_data.extend(chooser.bytes(4));
            let len = chooser.below(12);
            let text = chooser.text(len);
            push_string(&mut server_data, &text);
            server_data.extend(chooser.bytes(40));
            out.extend(server_data);
        }
        26 => {
            out.push(40);
            out.push(chooser.below(40) as u8);
            out.extend(chooser.bytes(4));
            let len = chooser.below(30);
            let mut text = b"\\name\\".to_vec();
            text.extend(chooser.text(len));
            push_string(out, &text);
        }
        27 => {
            out.push(if chooser.below(2) == 0 { 45 } else { 46 });
            out.push(chooser.u8());
            for _ in 0..chooser.below(4) {
                let len = 1 + chooser.below(10);
                let text = chooser.text(len);
                push_string(out, &text);
            }
            out.push(0);
            out.push(chooser.u8());
        }
        28 => {
            out.push(52);
            push_string(
                out,
                if chooser.below(2) == 0 {
                    b"epoch"
                } else {
                    b"k"
                },
            );
            let len = chooser.below(10);
            let text = chooser.text(len);
            push_string(out, &text);
        }
        29 => {
            out.push(51);
            out.push(chooser.u8());
            push_string(out, b"k");
            push_string(out, b"v");
        }
        30 => {
            // FTE spawnbaseline2 / spawnbaseline
            if chooser.below(2) == 0 {
                out.push(22);
                out.extend(chooser.bytes(6 + 3 * (coord_len + protocol.angle_size())));
            } else {
                out.push(66);
                let entity = chooser.below(512) as u16;
                delta(chooser, protocol, out, entity);
            }
        }
        31 => {
            out.push(41);
            let size = chooser.below(20) as u16;
            out.extend_from_slice(&size.to_le_bytes());
            out.push(chooser.u8());
            out.extend(chooser.bytes(size as usize));
        }
        32 => {
            out.push(10);
            out.extend(chooser.bytes(1 + 3 * protocol.angle_size()));
        }
        33 => {
            out.push(30);
            out.extend(chooser.bytes(3 * protocol.wide_coord_size() + 3 * protocol.angle_size()));
        }
        34 => {
            // a random command byte with random bytes
            out.push(chooser.u8());
            let len = chooser.below(8);
            out.extend(chooser.bytes(len));
        }
        35 => {
            out.push([1u8, 27, 28, 34, 35][chooser.below(5)]);
        }
        _ => {
            out.push([24u8, 32, 44, 49, 50, 12, 19, 20, 29, 39][chooser.below(10)]);
            let last = *out.last().unwrap();
            match last {
                24 | 32 | 44 => out.push(chooser.u8()),
                49 | 50 => out.extend(chooser.bytes(4)),
                12 => {
                    out.push(chooser.u8());
                    push_string(out, b"m");
                }
                19 => out.extend(chooser.bytes(2 + 3 * coord_len)),
                20 => out.extend(chooser.bytes(4 + 3 * (coord_len + protocol.angle_size()))),
                29 => out.extend(chooser.bytes(3 * coord_len + 3)),
                _ => out.extend(chooser.bytes(2)),
            }
        }
    }
}

fn hidden(chooser: &mut Chooser, out: &mut Vec<u8>) {
    if chooser.below(6) == 0 {
        // mvdsv's bare paused-duration payload
        out.extend_from_slice(&[0x0a, 0, chooser.u8()]);
        return;
    }
    for _ in 0..1 + chooser.below(4) {
        let (block_type, body): (u16, Vec<u8>) = match chooser.below(8) {
            0 | 1 => {
                let mut body = (chooser.below(4) as u16).to_le_bytes().to_vec();
                let len = chooser.below(50);
                body.extend(chooser.text(len));
                (3, body)
            }
            2 => (7, chooser.bytes(8)),
            3 => (0x0a, vec![chooser.u8()]),
            4 => (
                0x0b,
                vec![
                    0x80 | chooser.u8(),
                    0x80 | chooser.u8(),
                    chooser.u8() & 0x7f,
                ],
            ),
            5 => {
                let len = chooser.below(10);
                (chooser.u16(), chooser.bytes(len))
            }
            7 => {
                // a usercmd: angles mostly on the 360/65536 grid
                let mut body = vec![chooser.below(8) as u8, chooser.below(2) as u8, 13];
                for _ in 0..3 {
                    let angle = if chooser.below(8) == 0 {
                        chooser.u32()
                    } else {
                        ((chooser.u16() as i16 as f32) * (45.0 / 8192.0)).to_bits()
                    };
                    body.extend_from_slice(&angle.to_le_bytes());
                }
                body.extend(chooser.bytes(8));
                (1, body)
            }
            _ => {
                let len = chooser.below(10);
                (7, chooser.bytes(len))
            }
        };
        let is_length_lie = chooser.below(16) == 0;
        let body_len = if is_length_lie {
            chooser.u32() % 64
        } else {
            body.len() as u32
        };
        out.extend_from_slice(&body_len.to_le_bytes());
        out.extend_from_slice(&block_type.to_le_bytes());
        out.extend(body);
    }
    if chooser.below(8) == 0 {
        let len = chooser.below(5);
        out.extend(chooser.bytes(len));
    }
}

pub fn demo(bytes: &[u8]) -> Vec<u8> {
    let mut chooser = Chooser::new(bytes);
    let mut protocol = ProtocolState::default();
    let mut out = Vec::new();
    while !chooser.is_done() {
        let time_delta = if chooser.below(4) == 0 {
            chooser.u8()
        } else {
            chooser.below(30) as u8
        };
        let kind = chooser.below(20);
        let target = (chooser.below(8) as u8) << 3;
        match kind {
            0 => {
                // dem_set
                out.push(time_delta);
                out.push(2 | target);
                out.extend(chooser.bytes(8));
            }
            1..=3 => {
                // hidden
                let mut payload = Vec::new();
                hidden(&mut chooser, &mut payload);
                out.push(time_delta);
                out.push(3);
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                out.extend(payload);
            }
            4 => {
                // raw garbage (ends framing, or not)
                let len = chooser.below(16);
                out.extend(chooser.bytes(len));
            }
            5 => {
                // empty payload frame / zero-length
                out.push(time_delta);
                out.push([1u8, 4, 5, 6, 3][chooser.below(5)] | target);
                if out.last().unwrap() & 7 == 3 {
                    out.extend(chooser.bytes(4));
                }
                out.extend_from_slice(&0u32.to_le_bytes());
            }
            _ => {
                let frame_type = [1u8, 3, 4, 5, 6, 6, 6, 4][chooser.below(8)];
                let mut payload = Vec::new();
                for _ in 0..1 + chooser.below(8) {
                    command(&mut chooser, &mut protocol, &mut payload);
                }
                out.push(time_delta);
                if frame_type == 3 {
                    out.push(3);
                    let mask = chooser.u32() | 1;
                    out.extend_from_slice(&mask.to_le_bytes());
                } else {
                    out.push(frame_type | target);
                }
                out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                out.extend(payload);
            }
        }
    }
    out
}
