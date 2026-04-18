use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Annotation {
    pub display: String,
    pub fqn: String,
}

pub type MethodAnnotations = HashMap<String, Vec<Vec<Annotation>>>;

pub fn extract_annotations(jar_path: &Path, class_entry: &str) -> Option<MethodAnnotations> {
    let file = File::open(jar_path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name(class_entry).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    parse_class_annotations(&buf)
}

fn parse_class_annotations(data: &[u8]) -> Option<MethodAnnotations> {
    let mut r = Cursor::new(data);
    let magic = read_u32(&mut r)?;
    if magic != 0xCAFEBABE {
        return None;
    }
    let _minor = read_u16(&mut r)?;
    let _major = read_u16(&mut r)?;

    let cp_count = read_u16(&mut r)?;
    let cp = parse_constant_pool(&mut r, cp_count)?;

    let _access = read_u16(&mut r)?;
    let _this = read_u16(&mut r)?;
    let _super = read_u16(&mut r)?;

    let iface_count = read_u16(&mut r)?;
    for _ in 0..iface_count {
        read_u16(&mut r)?;
    }

    skip_members(&mut r, &cp)?;

    let method_count = read_u16(&mut r)?;
    let mut result = HashMap::new();
    for _ in 0..method_count {
        let _access = read_u16(&mut r)?;
        let name_idx = read_u16(&mut r)?;
        let _desc_idx = read_u16(&mut r)?;
        let method_name = cp_utf8(&cp, name_idx)?;

        let attr_count = read_u16(&mut r)?;
        let mut param_annots: Option<Vec<Vec<Annotation>>> = None;
        for _ in 0..attr_count {
            let attr_name_idx = read_u16(&mut r)?;
            let attr_len = read_u32(&mut r)? as usize;
            let attr_name = cp_utf8(&cp, attr_name_idx).unwrap_or_default();

            if attr_name == "RuntimeVisibleParameterAnnotations"
                || attr_name == "RuntimeInvisibleParameterAnnotations"
            {
                if let Some(annots) = parse_param_annotations(&mut r, &cp) {
                    let existing = param_annots.get_or_insert_with(Vec::new);
                    while existing.len() < annots.len() {
                        existing.push(Vec::new());
                    }
                    for (i, a) in annots.into_iter().enumerate() {
                        existing[i].extend(a);
                    }
                } else {
                    skip_bytes(&mut r, attr_len)?;
                }
            } else {
                skip_bytes(&mut r, attr_len)?;
            }
        }
        if let Some(annots) = param_annots {
            if annots.iter().any(|a| !a.is_empty()) {
                result.insert(method_name, annots);
            }
        }
    }
    Some(result)
}

fn parse_param_annotations(r: &mut Cursor<&[u8]>, cp: &[CpEntry]) -> Option<Vec<Vec<Annotation>>> {
    let num_params = read_u8(r)?;
    let mut params = Vec::with_capacity(num_params as usize);
    for _ in 0..num_params {
        let num_annots = read_u16(r)?;
        let mut annots = Vec::new();
        for _ in 0..num_annots {
            if let Some(fqn) = parse_annotation(r, cp) {
                let simple = fqn.rsplit('.').next().unwrap_or(&fqn);
                annots.push(Annotation {
                    display: format!("@{simple}"),
                    fqn,
                });
            }
        }
        params.push(annots);
    }
    Some(params)
}

fn parse_annotation(r: &mut Cursor<&[u8]>, cp: &[CpEntry]) -> Option<String> {
    let type_idx = read_u16(r)?;
    let num_pairs = read_u16(r)?;
    for _ in 0..num_pairs {
        let _name_idx = read_u16(r)?;
        skip_element_value(r)?;
    }
    let desc = cp_utf8(cp, type_idx)?;
    let inner = desc.strip_prefix('L')?.strip_suffix(';')?;
    Some(inner.replace('/', "."))
}

fn skip_element_value(r: &mut Cursor<&[u8]>) -> Option<()> {
    let tag = read_u8(r)?;
    match tag {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b's' => {
            read_u16(r)?;
        }
        b'e' => {
            read_u16(r)?;
            read_u16(r)?;
        }
        b'c' => {
            read_u16(r)?;
        }
        b'@' => {
            parse_annotation(r, &[])?;
        }
        b'[' => {
            let count = read_u16(r)?;
            for _ in 0..count {
                skip_element_value(r)?;
            }
        }
        _ => return None,
    }
    Some(())
}

#[derive(Clone)]
enum CpEntry {
    Utf8(String),
    Other,
}

fn parse_constant_pool(r: &mut Cursor<&[u8]>, count: u16) -> Option<Vec<CpEntry>> {
    let mut pool = vec![CpEntry::Other];
    let mut i = 1;
    while i < count {
        let tag = read_u8(r)?;
        match tag {
            1 => {
                let len = read_u16(r)? as usize;
                let pos = r.position() as usize;
                let data = r.get_ref();
                if pos + len > data.len() {
                    return None;
                }
                let s = String::from_utf8_lossy(&data[pos..pos + len]).to_string();
                r.set_position((pos + len) as u64);
                pool.push(CpEntry::Utf8(s));
            }
            3 | 4 => {
                skip_bytes(r, 4)?;
                pool.push(CpEntry::Other);
            }
            5 | 6 => {
                skip_bytes(r, 8)?;
                pool.push(CpEntry::Other);
                pool.push(CpEntry::Other);
                i += 1;
            }
            7 | 8 | 16 | 19 | 20 => {
                skip_bytes(r, 2)?;
                pool.push(CpEntry::Other);
            }
            9 | 10 | 11 | 12 | 17 | 18 => {
                skip_bytes(r, 4)?;
                pool.push(CpEntry::Other);
            }
            15 => {
                skip_bytes(r, 3)?;
                pool.push(CpEntry::Other);
            }
            _ => return None,
        }
        i += 1;
    }
    Some(pool)
}

fn skip_members(r: &mut Cursor<&[u8]>, _cp: &[CpEntry]) -> Option<()> {
    let count = read_u16(r)?;
    for _ in 0..count {
        read_u16(r)?;
        read_u16(r)?;
        read_u16(r)?;
        let attr_count = read_u16(r)?;
        for _ in 0..attr_count {
            read_u16(r)?;
            let len = read_u32(r)? as usize;
            skip_bytes(r, len)?;
        }
    }
    Some(())
}

fn cp_utf8(cp: &[CpEntry], idx: u16) -> Option<String> {
    match cp.get(idx as usize)? {
        CpEntry::Utf8(s) => Some(s.clone()),
        _ => None,
    }
}

fn read_u8(r: &mut Cursor<&[u8]>) -> Option<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf).ok()?;
    Some(buf[0])
}

fn read_u16(r: &mut Cursor<&[u8]>) -> Option<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).ok()?;
    Some(u16::from_be_bytes(buf))
}

fn read_u32(r: &mut Cursor<&[u8]>) -> Option<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).ok()?;
    Some(u32::from_be_bytes(buf))
}

fn skip_bytes(r: &mut Cursor<&[u8]>, n: usize) -> Option<()> {
    let pos = r.position() as usize;
    if pos + n > r.get_ref().len() {
        return None;
    }
    r.set_position((pos + n) as u64);
    Some(())
}
