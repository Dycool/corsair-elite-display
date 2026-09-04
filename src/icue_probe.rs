use std::fs;
use std::path::{Path, PathBuf};

const TARGET_DLL: &str = "iD_BD_x64_cc021.dll";
const REPORT_FILE: &str = "corsair-elite-display-icue-flash-read-probe.txt";
const INTERESTING_WORDS: &[&str] = &[
    "read",
    "get",
    "background",
    "hardware",
    "animation",
    "screen",
    "flash",
    "image",
    "memory",
    "storage",
    "download",
    "upload",
    "segment",
    "customized",
];

#[derive(Clone, Debug)]
struct Section {
    name: String,
    virtual_address: u32,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

#[derive(Clone, Debug)]
struct Export {
    name: String,
    rva: u32,
    ordinal: u16,
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| format!("PE read past end at 0x{offset:x}"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format!("PE read past end at 0x{offset:x}"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_c_string(bytes: &[u8], offset: usize) -> Result<String, String> {
    if offset >= bytes.len() {
        return Err(format!("string offset 0x{offset:x} is outside file"));
    }
    let end = bytes[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|index| offset + index)
        .unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[offset..end]).into_owned())
}

fn rva_to_offset(rva: u32, sections: &[Section]) -> Option<usize> {
    sections.iter().find_map(|section| {
        let span = section.virtual_size.max(section.raw_size);
        let end = section.virtual_address.checked_add(span)?;
        if rva >= section.virtual_address && rva < end {
            let within = rva - section.virtual_address;
            section
                .raw_offset
                .checked_add(within)
                .map(|offset| offset as usize)
        } else {
            None
        }
    })
}

fn parse_pe(bytes: &[u8]) -> Result<(Vec<Section>, Vec<Export>), String> {
    if bytes.get(0..2) != Some(b"MZ") {
        return Err("File is not an MZ/PE image".into());
    }

    let pe_offset = read_u32(bytes, 0x3c)? as usize;
    if bytes.get(pe_offset..pe_offset + 4) != Some(b"PE\0\0") {
        return Err("PE signature was not found".into());
    }

    let section_count = read_u16(bytes, pe_offset + 6)? as usize;
    let optional_size = read_u16(bytes, pe_offset + 20)? as usize;
    let optional_offset = pe_offset + 24;
    let magic = read_u16(bytes, optional_offset)?;
    let data_directory_offset = match magic {
        0x20b => optional_offset + 112, // PE32+
        0x10b => optional_offset + 96,  // PE32
        other => return Err(format!("Unsupported PE optional-header magic 0x{other:04x}")),
    };

    let export_rva = read_u32(bytes, data_directory_offset)?;
    let _export_size = read_u32(bytes, data_directory_offset + 4)?;

    let section_offset = optional_offset + optional_size;
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let offset = section_offset + index * 40;
        let name_bytes = bytes
            .get(offset..offset + 8)
            .ok_or_else(|| "Section table is truncated".to_string())?;
        let name_end = name_bytes.iter().position(|byte| *byte == 0).unwrap_or(8);
        let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();
        sections.push(Section {
            name,
            virtual_size: read_u32(bytes, offset + 8)?,
            virtual_address: read_u32(bytes, offset + 12)?,
            raw_size: read_u32(bytes, offset + 16)?,
            raw_offset: read_u32(bytes, offset + 20)?,
        });
    }

    if export_rva == 0 {
        return Ok((sections, Vec::new()));
    }
    let export_offset = rva_to_offset(export_rva, &sections)
        .ok_or_else(|| format!("Could not map export RVA 0x{export_rva:08x}"))?;

    let ordinal_base = read_u32(bytes, export_offset + 16)?;
    let function_count = read_u32(bytes, export_offset + 20)? as usize;
    let name_count = read_u32(bytes, export_offset + 24)? as usize;
    let functions_rva = read_u32(bytes, export_offset + 28)?;
    let names_rva = read_u32(bytes, export_offset + 32)?;
    let ordinals_rva = read_u32(bytes, export_offset + 36)?;

    let functions_offset = rva_to_offset(functions_rva, &sections)
        .ok_or_else(|| "Could not map export function table".to_string())?;
    let names_offset = rva_to_offset(names_rva, &sections)
        .ok_or_else(|| "Could not map export name table".to_string())?;
    let ordinals_offset = rva_to_offset(ordinals_rva, &sections)
        .ok_or_else(|| "Could not map export ordinal table".to_string())?;

    let mut exports = Vec::with_capacity(name_count);
    for index in 0..name_count {
        let name_rva = read_u32(bytes, names_offset + index * 4)?;
        let name_offset = rva_to_offset(name_rva, &sections)
            .ok_or_else(|| format!("Could not map export-name RVA 0x{name_rva:08x}"))?;
        let name = read_c_string(bytes, name_offset)?;
        let ordinal_index = read_u16(bytes, ordinals_offset + index * 2)? as usize;
        if ordinal_index >= function_count {
            continue;
        }
        let function_rva = read_u32(bytes, functions_offset + ordinal_index * 4)?;
        exports.push(Export {
            name,
            rva: function_rva,
            ordinal: ordinal_base.saturating_add(ordinal_index as u32) as u16,
        });
    }
    exports.sort_by(|left, right| left.name.cmp(&right.name));
    Ok((sections, exports))
}

fn looks_interesting(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    INTERESTING_WORDS.iter().any(|word| lower.contains(word))
}

fn collect_ascii_strings(bytes: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let printable = (0x20..=0x7e).contains(&byte);
        match (start, printable) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                if index - begin >= 4 {
                    let text = String::from_utf8_lossy(&bytes[begin..index]).into_owned();
                    if looks_interesting(&text) {
                        result.push(format!("ASCII @ 0x{begin:08x}: {text}"));
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        if bytes.len() - begin >= 4 {
            let text = String::from_utf8_lossy(&bytes[begin..]).into_owned();
            if looks_interesting(&text) {
                result.push(format!("ASCII @ 0x{begin:08x}: {text}"));
            }
        }
    }
    result
}

fn collect_utf16_ascii_strings(bytes: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    for alignment in 0..=1 {
        let mut index = alignment;
        let mut start = None;
        let mut text = String::new();
        while index + 1 < bytes.len() {
            let lo = bytes[index];
            let hi = bytes[index + 1];
            let printable = hi == 0 && (0x20..=0x7e).contains(&lo);
            if printable {
                if start.is_none() {
                    start = Some(index);
                    text.clear();
                }
                text.push(lo as char);
            } else if let Some(begin) = start.take() {
                if text.len() >= 4 && looks_interesting(&text) {
                    result.push(format!("UTF16 @ 0x{begin:08x}: {text}"));
                }
                text.clear();
            }
            index += 2;
        }
        if let Some(begin) = start {
            if text.len() >= 4 && looks_interesting(&text) {
                result.push(format!("UTF16 @ 0x{begin:08x}: {text}"));
            }
        }
    }
    result
}

fn hex_dump(bytes: &[u8], offset: usize, length: usize) -> String {
    let end = offset.saturating_add(length).min(bytes.len());
    let mut output = String::new();
    let mut cursor = offset;
    while cursor < end {
        let line_end = (cursor + 16).min(end);
        output.push_str(&format!("{cursor:08x}:"));
        for byte in &bytes[cursor..line_end] {
            output.push_str(&format!(" {byte:02x}"));
        }
        output.push('\n');
        cursor = line_end;
    }
    output
}

fn candidate_dll_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let root = PathBuf::from(program_files).join("Corsair");
        candidates.push(root.join("Corsair iCUE5 Software").join(TARGET_DLL));
        candidates.push(root.join("CORSAIR iCUE 4 Software").join(TARGET_DLL));
        candidates.push(root.join("Corsair iCUE 4 Software").join(TARGET_DLL));
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    candidates.push(path.join(TARGET_DLL));
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn find_dll() -> Result<PathBuf, String> {
    candidate_dll_paths()
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Could not find {TARGET_DLL} under the Corsair Program Files directory"))
}

fn section_for_rva<'a>(rva: u32, sections: &'a [Section]) -> Option<&'a Section> {
    sections.iter().find(|section| {
        let span = section.virtual_size.max(section.raw_size);
        rva >= section.virtual_address
            && rva < section.virtual_address.saturating_add(span)
    })
}

fn build_report(path: &Path, bytes: &[u8]) -> Result<String, String> {
    let (sections, exports) = parse_pe(bytes)?;
    let mut report = String::new();
    report.push_str("Corsair Elite Display - read-only iCUE flash-read reverse-engineering probe\n");
    report.push_str("IMPORTANT: this probe only reads the DLL file. It sends NO commands to the cooler.\n\n");
    report.push_str(&format!("DLL: {}\n", path.display()));
    report.push_str(&format!("Size: {} bytes\n\n", bytes.len()));

    report.push_str("PE sections:\n");
    for section in &sections {
        report.push_str(&format!(
            "  {:<8} RVA=0x{:08x} virtual=0x{:x} raw_off=0x{:08x} raw_size=0x{:x}\n",
            section.name,
            section.virtual_address,
            section.virtual_size,
            section.raw_offset,
            section.raw_size
        ));
    }

    report.push_str("\nExports:\n");
    for export in &exports {
        report.push_str(&format!(
            "  ordinal={} RVA=0x{:08x} {}\n",
            export.ordinal, export.rva, export.name
        ));
    }

    report.push_str("\nInteresting exports:\n");
    let mut interesting_exports = Vec::new();
    for export in &exports {
        if looks_interesting(&export.name) {
            interesting_exports.push(export);
            let section = section_for_rva(export.rva, &sections)
                .map(|section| section.name.as_str())
                .unwrap_or("?");
            report.push_str(&format!(
                "  RVA=0x{:08x} section={} {}\n",
                export.rva, section, export.name
            ));
        }
    }
    if interesting_exports.is_empty() {
        report.push_str("  (none)\n");
    }

    report.push_str("\nInteresting embedded strings:\n");
    let mut strings = collect_ascii_strings(bytes);
    strings.extend(collect_utf16_ascii_strings(bytes));
    strings.sort();
    strings.dedup();
    for line in &strings {
        report.push_str("  ");
        report.push_str(line);
        report.push('\n');
    }
    if strings.is_empty() {
        report.push_str("  (none)\n");
    }

    report.push_str("\nFunction bytes for flash/media-related exports (up to 768 bytes each):\n");
    for export in interesting_exports {
        let lower = export.name.to_ascii_lowercase();
        if !(lower.contains("background")
            || lower.contains("animation")
            || lower.contains("screen")
            || lower.contains("flash")
            || lower.contains("image"))
        {
            continue;
        }
        report.push_str(&format!("\n== {} @ RVA 0x{:08x} ==\n", export.name, export.rva));
        if let Some(offset) = rva_to_offset(export.rva, &sections) {
            report.push_str(&hex_dump(bytes, offset, 768));
        } else {
            report.push_str("Could not map RVA to file offset.\n");
        }
    }

    Ok(report)
}

pub fn run() -> Result<PathBuf, String> {
    let dll = find_dll()?;
    let bytes = fs::read(&dll).map_err(|error| format!("Could not read {}: {error}", dll.display()))?;
    let report = build_report(&dll, &bytes)?;
    let output = std::env::temp_dir().join(REPORT_FILE);
    fs::write(&output, report)
        .map_err(|error| format!("Could not write {}: {error}", output.display()))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interesting_name_filter_finds_media_operations() {
        assert!(looks_interesting("iD_USB_update_hardware_mode_background_cc021"));
        assert!(looks_interesting("get_screen_config"));
        assert!(!looks_interesting("close_device"));
    }
}
