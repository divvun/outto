//! Embed and read custom segments in thin 64-bit Mach-O binaries.
//!
//! `embed_segment` adds a fresh `LC_SEGMENT_64` load command and writes the
//! payload bytes after `__LINKEDIT`. `find_segment` is the runtime-side
//! counterpart: open the current executable, walk the load commands, return
//! `(file_offset, size)` for the named segment.
//!
//! Only thin arm64 / x86_64 Mach-O binaries are supported; fat (universal)
//! binaries are rejected with a clear error. The cargo builds outto ships
//! today are all thin, so this is fine for v1.
//!
//! Mach-O layout references: `/usr/include/mach-o/loader.h`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

// --- Constants ---

const MH_MAGIC_64: u32 = 0xFEEDFACF;
const MH_CIGAM_64: u32 = 0xCFFAEDFE;
const FAT_MAGIC: u32 = 0xCAFEBABE;
const FAT_CIGAM: u32 = 0xBEBAFECA;
const FAT_MAGIC_64: u32 = 0xCAFEBABF;
const FAT_CIGAM_64: u32 = 0xBFBAFECA;

const LC_SEGMENT_64: u32 = 0x19;

const HEADER_SIZE_64: u64 = 32;
const SEGMENT_64_BASE_SIZE: u32 = 72;
const SECTION_64_SIZE: u32 = 80;

const VM_PROT_READ: i32 = 0x01;
const S_REGULAR: u32 = 0x0;

/// Describes an existing segment. We only need the location + name for
/// `find_segment` and `embed_segment` layout decisions.
#[derive(Debug, Clone, Copy)]
struct SegmentInfo {
    fileoff: u64,
    filesize: u64,
    segname: [u8; 16],
}

/// Find a named segment in a Mach-O binary. Returns `(file_offset, size)` for
/// the first section inside the matching segment. If you just need the segment
/// bytes (one section), this is what you want.
pub fn find_segment(mach_o: &Path, name: &str) -> io::Result<Option<(u64, u64)>> {
    let mut f = File::open(mach_o)?;

    let magic = read_u32(&mut f)?;
    reject_fat(magic)?;
    let (needs_swap, _) = parse_magic(magic)?;

    f.seek(SeekFrom::Start(0))?;
    let header = read_header(&mut f, needs_swap)?;

    let name_bytes = pad_segname(name);

    for seg in iter_segments(&mut f, &header, needs_swap)? {
        if seg.segname == name_bytes {
            // Return the entire segment's fileoff/filesize.
            // For our use case each __OUTTO segment has exactly one section,
            // which covers the whole segment region.
            return Ok(Some((seg.fileoff, seg.filesize)));
        }
    }

    Ok(None)
}

/// Embed `data` as a new named segment with a single section called `__payload`
/// at the end of the Mach-O file (after `__LINKEDIT`).
///
/// Call this BEFORE `codesign` — codesign will rewrite `LC_CODE_SIGNATURE`
/// to cover the new segment and the new load command.
pub fn embed_segment(mach_o: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().read(true).write(true).open(mach_o)?;

    let magic = read_u32(&mut f)?;
    reject_fat(magic)?;
    let (needs_swap, _) = parse_magic(magic)?;

    f.seek(SeekFrom::Start(0))?;
    let header = read_header(&mut f, needs_swap)?;

    // Find highest file offset in existing segments → where we can safely append.
    let segments = iter_segments(&mut f, &header, needs_swap)?;
    let tail = segments
        .iter()
        .map(|s| s.fileoff + s.filesize)
        .max()
        .unwrap_or(HEADER_SIZE_64 + header.sizeofcmds as u64);

    // Page-align the start of our payload. Default to 16K pages (arm64).
    let page = page_size_for_cpu(header.cputype);
    let new_data_offset = align_up(tail, page);

    // End of existing load-command area.
    let end_of_cmds = HEADER_SIZE_64 + header.sizeofcmds as u64;

    // Do we have room for a new 152-byte LC in the header padding?
    // First segment's fileoff is the earliest data in the file. Anything
    // between end_of_cmds and first_segment_fileoff is padding.
    let first_fileoff = segments
        .iter()
        .filter(|s| s.fileoff > 0) // skip __PAGEZERO (fileoff=0, filesize=0)
        .map(|s| s.fileoff)
        .min()
        .unwrap_or(0x1000);

    let new_cmd_size: u32 = SEGMENT_64_BASE_SIZE + SECTION_64_SIZE;
    if end_of_cmds + new_cmd_size as u64 > first_fileoff {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "Not enough room for a new load command: end_of_cmds={} + {} > first_fileoff={}",
                end_of_cmds, new_cmd_size, first_fileoff
            ),
        ));
    }

    // Append payload bytes.
    f.seek(SeekFrom::Start(new_data_offset))?;
    f.write_all(data)?;

    // Write new LC_SEGMENT_64 + one section_64 at end_of_cmds.
    let segname = pad_segname(name);
    let sectname = pad_segname("__payload");

    f.seek(SeekFrom::Start(end_of_cmds))?;

    write_u32_sw(&mut f, LC_SEGMENT_64, needs_swap)?;
    write_u32_sw(&mut f, new_cmd_size, needs_swap)?;
    f.write_all(&segname)?;
    write_u64_sw(&mut f, new_data_offset, needs_swap)?; // vmaddr = file offset (arbitrary; not loaded into memory at a specific VM address)
    write_u64_sw(&mut f, align_up(data.len() as u64, page), needs_swap)?; // vmsize
    write_u64_sw(&mut f, new_data_offset, needs_swap)?; // fileoff
    write_u64_sw(&mut f, data.len() as u64, needs_swap)?; // filesize
    write_i32_sw(&mut f, VM_PROT_READ, needs_swap)?; // maxprot
    write_i32_sw(&mut f, VM_PROT_READ, needs_swap)?; // initprot
    write_u32_sw(&mut f, 1, needs_swap)?; // nsects
    write_u32_sw(&mut f, 0, needs_swap)?; // flags

    // section_64
    f.write_all(&sectname)?;
    f.write_all(&segname)?;
    write_u64_sw(&mut f, new_data_offset, needs_swap)?; // addr
    write_u64_sw(&mut f, data.len() as u64, needs_swap)?; // size
    write_u32_sw(&mut f, new_data_offset as u32, needs_swap)?; // offset
    write_u32_sw(&mut f, 12, needs_swap)?; // align (2^12 = 4096 — legal, codesign accepts)
    write_u32_sw(&mut f, 0, needs_swap)?; // reloff
    write_u32_sw(&mut f, 0, needs_swap)?; // nreloc
    write_u32_sw(&mut f, S_REGULAR, needs_swap)?; // flags
    write_u32_sw(&mut f, 0, needs_swap)?; // reserved1
    write_u32_sw(&mut f, 0, needs_swap)?; // reserved2
    write_u32_sw(&mut f, 0, needs_swap)?; // reserved3

    // Update header: ncmds += 1, sizeofcmds += new_cmd_size.
    f.seek(SeekFrom::Start(16))?; // offset of ncmds field
    write_u32_sw(&mut f, header.ncmds + 1, needs_swap)?;
    write_u32_sw(&mut f, header.sizeofcmds + new_cmd_size, needs_swap)?;

    f.flush()?;
    Ok(())
}

// --- Internal helpers ---

struct MachHeader64 {
    cputype: i32,
    #[allow(dead_code)]
    cpusubtype: i32,
    #[allow(dead_code)]
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    #[allow(dead_code)]
    flags: u32,
}

fn read_header<R: Read + Seek>(f: &mut R, swap: bool) -> io::Result<MachHeader64> {
    f.seek(SeekFrom::Start(0))?;
    let _magic = read_u32(f)?; // already validated by caller
    let cputype = read_i32_sw(f, swap)?;
    let cpusubtype = read_i32_sw(f, swap)?;
    let filetype = read_u32_sw(f, swap)?;
    let ncmds = read_u32_sw(f, swap)?;
    let sizeofcmds = read_u32_sw(f, swap)?;
    let flags = read_u32_sw(f, swap)?;
    let _reserved = read_u32_sw(f, swap)?;
    Ok(MachHeader64 {
        cputype,
        cpusubtype,
        filetype,
        ncmds,
        sizeofcmds,
        flags,
    })
}

fn iter_segments<R: Read + Seek>(
    f: &mut R,
    header: &MachHeader64,
    swap: bool,
) -> io::Result<Vec<SegmentInfo>> {
    let mut out = Vec::new();
    let mut cursor = HEADER_SIZE_64;

    for _ in 0..header.ncmds {
        f.seek(SeekFrom::Start(cursor))?;
        let cmd = read_u32_sw(f, swap)?;
        let cmd_size = read_u32_sw(f, swap)?;

        if cmd == LC_SEGMENT_64 {
            let mut segname = [0u8; 16];
            f.read_exact(&mut segname)?;
            let _vmaddr = read_u64_sw(f, swap)?;
            let _vmsize = read_u64_sw(f, swap)?;
            let fileoff = read_u64_sw(f, swap)?;
            let filesize = read_u64_sw(f, swap)?;

            out.push(SegmentInfo {
                fileoff,
                filesize,
                segname,
            });
        }

        cursor += cmd_size as u64;
    }
    Ok(out)
}

fn parse_magic(magic: u32) -> io::Result<(bool, bool)> {
    // Returns (needs_swap, is_64bit).
    match magic {
        MH_MAGIC_64 => Ok((false, true)),
        MH_CIGAM_64 => Ok((true, true)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Mach-O magic: 0x{magic:08x}"),
        )),
    }
}

fn reject_fat(magic: u32) -> io::Result<()> {
    if magic == FAT_MAGIC || magic == FAT_CIGAM || magic == FAT_MAGIC_64 || magic == FAT_CIGAM_64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fat (universal) Mach-O binaries are not supported — build thin arm64 or x86_64 binaries",
        ));
    }
    Ok(())
}

fn page_size_for_cpu(cputype: i32) -> u64 {
    // CPU_TYPE_ARM64 = 0x0100000c, CPU_TYPE_X86_64 = 0x01000007
    match cputype {
        0x0100000c => 0x4000, // arm64: 16K pages
        0x01000007 => 0x1000, // x86_64: 4K pages
        _ => 0x1000,
    }
}

fn pad_segname(name: &str) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let src = name.as_bytes();
    let n = src.len().min(16);
    bytes[..n].copy_from_slice(&src[..n]);
    bytes
}

fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        value
    } else {
        (value + alignment - 1) & !(alignment - 1)
    }
}

fn read_u32<R: Read>(f: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u32_sw<R: Read>(f: &mut R, swap: bool) -> io::Result<u32> {
    let v = read_u32(f)?;
    Ok(if swap { v.swap_bytes() } else { v })
}

fn read_i32_sw<R: Read>(f: &mut R, swap: bool) -> io::Result<i32> {
    Ok(read_u32_sw(f, swap)? as i32)
}

fn read_u64_sw<R: Read>(f: &mut R, swap: bool) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    f.read_exact(&mut buf)?;
    let v = u64::from_le_bytes(buf);
    Ok(if swap { v.swap_bytes() } else { v })
}

fn write_u32_sw<W: Write>(f: &mut W, v: u32, swap: bool) -> io::Result<()> {
    let out = if swap { v.swap_bytes() } else { v };
    f.write_all(&out.to_le_bytes())
}

fn write_i32_sw<W: Write>(f: &mut W, v: i32, swap: bool) -> io::Result<()> {
    write_u32_sw(f, v as u32, swap)
}

fn write_u64_sw<W: Write>(f: &mut W, v: u64, swap: bool) -> io::Result<()> {
    let out = if swap { v.swap_bytes() } else { v };
    f.write_all(&out.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_segname_truncates_to_16() {
        let b = pad_segname("__VERY_LONG_SEGMENT_NAME");
        assert_eq!(&b[..16], b"__VERY_LONG_SEGM");
    }

    #[test]
    fn test_pad_segname_short() {
        let b = pad_segname("__OUTTO");
        assert_eq!(&b[..7], b"__OUTTO");
        assert_eq!(b[7], 0);
    }

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4096), 0);
        assert_eq!(align_up(1, 4096), 4096);
        assert_eq!(align_up(4096, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
    }

    #[test]
    fn test_reject_fat_binary() {
        let err = reject_fat(FAT_MAGIC).unwrap_err();
        assert!(err.to_string().contains("fat"));

        let err = reject_fat(FAT_CIGAM).unwrap_err();
        assert!(err.to_string().contains("fat"));

        // Thin magics are fine.
        assert!(reject_fat(MH_MAGIC_64).is_ok());
        assert!(reject_fat(MH_CIGAM_64).is_ok());
    }

    #[test]
    fn test_embed_and_find_roundtrip_on_self() {
        // Copy the current test binary to a temp path, embed a known payload,
        // then read it back. This is a real Mach-O produced by cargo.
        let exe = std::env::current_exe().unwrap();
        let tmp_dir = std::env::temp_dir().join(format!("outto-macho-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let target = tmp_dir.join("test-binary");
        std::fs::copy(&exe, &target).unwrap();

        let payload = b"hello from the __OUTTO segment";
        let sig = find_segment(&target, "__OUTTO").unwrap();
        assert!(
            sig.is_none(),
            "target binary shouldn't already have __OUTTO"
        );

        embed_segment(&target, "__OUTTO", payload).unwrap();

        let (offset, size) = find_segment(&target, "__OUTTO")
            .unwrap()
            .expect("segment should now exist");
        assert_eq!(size, payload.len() as u64);

        // Read back the bytes.
        let mut f = std::fs::File::open(&target).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        let mut buf = vec![0u8; payload.len()];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], payload);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
