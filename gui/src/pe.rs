//! Read COFF PE sections to find embedded payloads.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const DOS_MAGIC: u16 = 0x5A4D;
const PE_SIGNATURE: u32 = 0x00004550;
const PE32_MAGIC: u16 = 0x10B;
const PE32PLUS_MAGIC: u16 = 0x20B;

/// Find a named section in a PE file. Returns `(file_offset, size)` if found.
pub fn find_section(exe_path: &Path, section_name: &str) -> io::Result<Option<(u64, u64)>> {
    let mut f = File::open(exe_path)?;
    let info = parse_pe_info(&mut f)?;

    let name_bytes = pad_section_name(section_name);

    for i in 0..info.number_of_sections as u64 {
        let header_offset = info.section_table_offset + i * 40;
        f.seek(SeekFrom::Start(header_offset))?;

        let mut name = [0u8; 8];
        f.read_exact(&mut name)?;

        if name == name_bytes {
            let virtual_size = read_u32(&mut f)?; // actual data length
            let _virtual_address = read_u32(&mut f)?;
            let _size_of_raw_data = read_u32(&mut f)?; // padded, don't use for length
            let pointer_to_raw_data = read_u32(&mut f)?;

            return Ok(Some((pointer_to_raw_data as u64, virtual_size as u64)));
        }
    }

    Ok(None)
}

struct PeInfo {
    section_table_offset: u64,
    number_of_sections: u16,
}

fn parse_pe_info(f: &mut File) -> io::Result<PeInfo> {
    f.seek(SeekFrom::Start(0))?;
    let dos_magic = read_u16(f)?;
    if dos_magic != DOS_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a valid PE file",
        ));
    }

    f.seek(SeekFrom::Start(0x3C))?;
    let e_lfanew = read_u32(f)? as u64;

    f.seek(SeekFrom::Start(e_lfanew))?;
    let pe_sig = read_u32(f)?;
    if pe_sig != PE_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a valid PE file",
        ));
    }

    let coff_header_offset = e_lfanew + 4;

    f.seek(SeekFrom::Start(coff_header_offset))?;
    let _machine = read_u16(f)?;
    let number_of_sections = read_u16(f)?;
    f.seek(SeekFrom::Start(coff_header_offset + 16))?;
    let size_of_optional_header = read_u16(f)?;

    let optional_header_offset = coff_header_offset + 20;
    let section_table_offset = optional_header_offset + size_of_optional_header as u64;

    f.seek(SeekFrom::Start(optional_header_offset))?;
    let opt_magic = read_u16(f)?;
    if opt_magic != PE32_MAGIC && opt_magic != PE32PLUS_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unknown PE optional header",
        ));
    }

    Ok(PeInfo {
        section_table_offset,
        number_of_sections,
    })
}

fn pad_section_name(name: &str) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    let name_bytes = name.as_bytes();
    let len = name_bytes.len().min(8);
    bytes[..len].copy_from_slice(&name_bytes[..len]);
    bytes
}

fn read_u16(f: &mut File) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    f.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(f: &mut File) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}
