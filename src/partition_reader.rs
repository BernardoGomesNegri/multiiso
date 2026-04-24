use std::convert::identity;
use std::mem::MaybeUninit;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use uuid::{Uuid,uuid};


pub const ESP_UUID: Uuid = uuid!("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
pub const MS_BASIC_TYPE: Uuid = uuid!("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7");
pub const LINUX_DATA_TYPE: Uuid = uuid!("0FC63DAF-8483-4772-8E79-3D69D8477DE4");
pub const LBA_SIZE: u64 = 512;

fn temp_buf<T, B: Default + Copy, const N: usize, F: FnOnce([B; N]) -> T>(fun: F) -> T {
    let b = [B::default(); N];
    fun(b)
}

fn parse_file_bytes<T, const N: usize, F: FnOnce([u8; N]) -> T>(file: &mut File, parse: F) -> std::io::Result<T> {
    temp_buf(|mut b| file.read_exact(&mut b).map(|_| parse(b)))
}

fn collect_to_array<const N: usize, T: Copy, S: IntoIterator<Item = T>>(iter: S) -> [T; N] {
    let mut ret = [MaybeUninit::uninit(); N];
    let mut written_items = 0;
    for item in &mut ret[..].iter_mut().zip(iter) {
        item.0.write(item.1);
        written_items = written_items + 1;
    }
    if written_items != N {
        panic!("collect_to_array used incorrectly!");
    }
    // Once MaybeUninit::array_assume_init is stabilized we can use it
    unsafe {(&ret as *const _ as *const [T; N]).read()}
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Part {
    pub number: u8,
    pub start: u64,
    pub size: u64,
    pub type_uuid: Uuid,
    pub part_uuid: Uuid,
    pub name: String,
    pub flag_boot: bool,
    pub flag_esp: bool,
}

pub fn get_gpt_data(device: &Path) -> std::io::Result<Vec<Part>> {
    let mut file = File::open(device)?;
    // Check the signature
    file.seek(SeekFrom::Start(0x200))?;
    match parse_file_bytes(&mut file, identity) {
        Ok([0x45, 0x46, 0x49, 0x20, 0x50, 0x41, 0x52, 0x54]) => (),
        _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Did not find GPT signature"))
    }
    // LBA 1, "Starting LBA of partition entries" field.
    file.seek(SeekFrom::Start(0x200 + 0x48))?;
    let part_table_lba = parse_file_bytes(&mut file, u64::from_le_bytes)?;
    // "Number of partition entries in array" field.
    let part_entry_number = parse_file_bytes(&mut file, u32::from_le_bytes)?;
    // "Size of a partition entry" field
    let part_entry_size = parse_file_bytes(&mut file, u32::from_le_bytes)?;
    file.seek(SeekFrom::Start(0x200 * part_table_lba))?;
    let mut ret = Vec::new();
    for i in 0..part_entry_number {
        let part_num = i + 1;
        let type_uuid = parse_file_bytes(&mut file, uuid::Uuid::from_bytes_le)?;
        if type_uuid == Uuid::nil() {
            continue;
        }
        let part_uuid = parse_file_bytes(&mut file, uuid::Uuid::from_bytes_le)?;
        let first_lba = parse_file_bytes(&mut file, u64::from_le_bytes)?;
        let last_lba = parse_file_bytes(&mut file, u64::from_le_bytes)?;
        let _attributes = parse_file_bytes(&mut file, u64::from_le_bytes)?;
        let part_name_buf: [u16; 36] = parse_file_bytes(&mut file, |b: [u8; 72]| collect_to_array(b.as_chunks::<2>().0.into_iter().map(|bytes| u16::from_le_bytes(*bytes))))?;
        let part_name_str = String::from_utf16(&part_name_buf.into_iter().filter(|c| *c != 0).collect::<Vec<u16>>()).map_or("".to_owned(), identity);
        ret.push(Part {
            number: part_num as u8,
            start: first_lba * LBA_SIZE,
            size: (last_lba - first_lba + 1) * LBA_SIZE,
            type_uuid: type_uuid,
            part_uuid: part_uuid,
            flag_esp: type_uuid == ESP_UUID,
            flag_boot: false,
            name: part_name_str,
        });
        file.seek(SeekFrom::Current((128 - part_entry_size) as i64))?;
    }
    Ok(ret)
}

// Returns Ok(None) if protective MBR
pub fn get_mbr_data(device: &Path) -> std::io::Result<Option<Vec<Part>>> {
    let mut f = File::open(device)?;
    // The start of PartitionRecord
    f.seek(SeekFrom::Start(446))?;
    let mut ret = Vec::new();
    for i in 0..4 {
        let part_num = i + 1;
        let flag_boot = parse_file_bytes(&mut f, |b: [u8; 1]| (b[0] / 128) % 2 == 1)?;
        f.seek(SeekFrom::Current(3))?;
        let os_type = parse_file_bytes(&mut f, |b: [u8; 1]| b[0])?;
        if os_type == 0xEE {
            // This is a protective MBR
            return Ok(None);
        }
        f.seek(SeekFrom::Current(3))?;
        let start_lba = parse_file_bytes(&mut f, u32::from_le_bytes)?;
        let size_in_lba = parse_file_bytes(&mut f, u32::from_le_bytes)?;
        // An Os Type of zero usually also means the partition is disabled
        // Except, for some reason, isohybrid sets its partition types to zero
        if size_in_lba == 0 {
            continue;
        }
        let type_uuid = match os_type {
            0x07 | 0x0F | 0x27 => MS_BASIC_TYPE,
            0x82 | 0x84 => LINUX_DATA_TYPE,
            0xEF => ESP_UUID,
            // Just default to Microsoft
            _ => MS_BASIC_TYPE,
        };
        ret.push(Part {
            number: part_num,
            start: start_lba as u64 * LBA_SIZE,
            size: size_in_lba as u64 * LBA_SIZE,
            type_uuid: type_uuid,
            part_uuid: Uuid::nil(),
            name: "".to_owned(),
            flag_boot: flag_boot,
            flag_esp: type_uuid == ESP_UUID
        });
    }
    Ok(Some(ret))
}

// Uses MBR data, unless it is a protective MBR, in which case uses GPT
pub fn get_device_parts(iso: &Path) -> std::io::Result<Vec<Part>> {
    match get_mbr_data(iso) {
        Err(e) => Err(e),
        Ok(None) => get_gpt_data(iso),
        Ok(Some(parts)) => Ok(parts)
    }
}
