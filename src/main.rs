use std::ffi::{OsString, OsStr};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::path::Path;
use std::fs::{create_dir_all, copy, read_dir, write};
use std::env;
use std::fmt::Display;
use std::env::temp_dir;
use std::collections::HashMap;
use std::clone::Clone;
use uuid::Uuid;
mod partition_reader;
use partition_reader::*;

fn temp_buf<T, B: Default, F: FnOnce(&mut B) -> T>(fun: F) -> (B, T) {
    let mut buf = B::default();
    let res = fun(&mut buf);
    (buf, res)
}

fn parts_to_sfd_script(parts: &[Part]) -> String {
    let mut ret = String::new();
    ret.push_str("label: gpt\n");
    for part in parts {
        ret.push_str(&format!("size={}MiB, ", (part.size / (1024 * 1024)) + 1));
        if part.flag_esp {
            ret.push_str("bootable, ");
        }
        ret.push_str(&format!("uuid={}, ", part.part_uuid.hyphenated()));
        ret.push_str(&format!("type={}, ", part.type_uuid.hyphenated()));
        ret.push_str(&format!("name=\"{}\"", part.name));
        ret.push_str("\n");
    }
    ret
}

fn iso_part_to_disk(p: &Part) -> Part {
    Part {
        number: 0,
        start: 0,
        size: p.size,
        type_uuid: if p.type_uuid == Uuid::nil() {if p.flag_esp {ESP_UUID} else {MS_BASIC_TYPE}} else {p.type_uuid},
        part_uuid: Uuid::new_v4(),
        name: if p.name != "" {p.name.clone()} else {format!("part{}", p.number)},
        // Don't try to use legacy boot
        flag_boot: false,
        flag_esp: p.flag_esp
    }
}

fn write_menuentry(display_name: &str, part_uuid: &Uuid) -> String {
    format!("menuentry \"{display_name}\" {{
    volume {uuid}
    loader /EFI/BOOT/bootx64.efi
}}\n", uuid=part_uuid.hyphenated())
}

fn run_sfd_script(wipe: bool, device: &Path, script: &str) -> Result<(), String> {
    println!("Starting sfdisk on device {}", device.display());
    let mut sfd_cmd = Command::new("sfdisk");
    if !wipe {
        sfd_cmd.arg("--append");
    }
    let sfd_process = sfd_cmd.arg(device)
        .arg("--no-reread")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    match sfd_process {
        Err(e) => Err(format!("Could not spawn sfdisk: {}", e)),
        Ok(mut p) => {
            let mut stdin = p.stdin.take().expect("Wanted piped");
            stdin.write_all(script.as_bytes()).map_err(|e| format!("Could not write stdin: {}", e))?;
            stdin.flush().map_err(|e| format!("Could not flush stdin: {}", e))?;
            // Unblocks sfdisk
            drop(stdin);
            let status = p.wait();
            match status {
                Err(e) => Err(format!("Couldn't join sfdisk: {}", e)),
                Ok(s) => {
                    if !s.success() {
                        let (stderr_buf, read_stderr) = temp_buf(|b| p.stderr.expect("was piped").read_to_string(b));
                        let stderr_text = match read_stderr {
                            Err(e) => format!("could not read sfdisk stderr: {}", e),
                            Ok(_) => stderr_buf
                        };
                        Err(format!("sfdisk error: {}", stderr_text))
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }
}

fn run_cmd<S: AsRef<OsStr>>(command: &str, inherit_stdout: bool, args: &[S]) -> Result<(), String> {
    println!("Running {} {}", command, args.iter().map(|x| x.as_ref().display().to_string()).collect::<Vec<String>>().join(" "));
    let cmd_string = &command;
    let mut cmd = Command::new(cmd_string);
    cmd.args(args);
    cmd.stderr(Stdio::piped());
    if inherit_stdout {
        cmd.stdout(Stdio::inherit());
    }
    let result = cmd.spawn().map_err(|e| format!("Could not spawn {}: {}", command, e))?
    .wait_with_output().map_err(|e| format!("Could not wait for {}: {}", command, e))?;
    if !result.status.success() {
        Err(format!("Error with {}, exit status {}: {}", command, result.status.code().unwrap_or(-32000), String::from_utf8(result.stderr).unwrap_or("could not read stderr".to_owned())))
    } else {
        Ok(())
    }
}

fn rescan_device(device: &Path) -> Result<(), String> {
    run_cmd("blockdev", false, &["--rereadpt".as_ref(), device])?;
    run_cmd("udevadm", false, &["settle"])
}

fn dd_copy_data(source: &Path, destination: &Path, source_offset: u64, dest_offset: u64, size: u64) -> Result<(), String> {
    let mut if_argument: OsString = OsString::new();
    if_argument.push("if=");
    if_argument.push(source);
    let mut of_argument: OsString = OsString::new();
    of_argument.push("of=");
    of_argument.push(destination);
    run_cmd("dd", true, &[
        <OsString as AsRef<OsStr>>::as_ref(&if_argument),
        <OsString as AsRef<OsStr>>::as_ref(&of_argument),
        "bs=5M".as_ref(),
        "status=progress".as_ref(),
        &format!("seek={}B", dest_offset).as_ref(),
        &format!("skip={}B", source_offset).as_ref(),
        &format!("count={}B", size).as_ref()
    ])
}

fn copy_recurse(source: &Path, destination: &Path) -> std::io::Result<()> {
    for entry_r in read_dir(source)? {
        let entry = entry_r?;
        let file_type = entry.file_type()?;
        if file_type.is_file() {
            create_dir_all(destination)?;
            copy(entry.path(), destination.join(entry.file_name()))?;
        } else if file_type.is_dir() {
            let dest_dir = destination.join(entry.file_name());
            create_dir_all(dest_dir.clone())?;
            copy_recurse(entry.path().as_ref(), dest_dir.as_ref())?;
        }
    }
    Ok(())
}

fn install_mount_refind(device: &Path, mount: &Path, refind_files: &Path) -> Result<(), String> {
    let uuid_refind = Uuid::new_v4();
    run_sfd_script(true, device, &parts_to_sfd_script(&[Part {
        number: 0,
        start: 0,
        size: 4 * 1024 * 1024,
        type_uuid: ESP_UUID,
        part_uuid: uuid_refind,
        name: "rEFInd".to_owned(),
        flag_boot: false,
        flag_esp: true
    }]))?;
    // Make the kernel re-read the part table
    rescan_device(device)?;
    // Format it as FAT16
    let mut temp_buf: [u8; 40] = [0; 40];
    let esp_path: String = format!("/dev/disk/by-partuuid/{}", uuid_refind.hyphenated().encode_lower(&mut temp_buf));
    run_cmd("mkfs.fat", false, &[<str as AsRef<OsStr>>::as_ref(&esp_path)])?;
    // Now mount the volume
    run_cmd("mount", false, &[esp_path.as_ref(), mount])?;
    // Make the directories
    let refind_dir = mount.join("EFI/BOOT");
    create_dir_all(refind_dir.clone()).map_err(|e| format!("Could not create /EFI/BOOT: {}", e))?;
    let err_lambda = |e| format!("Error when copying rEFInd files: {}", e);
    // Copy the files to their correct location for x64
    println!("Installing rEFInd");
    copy_recurse(&refind_files.join("icons"), &refind_dir.join("icons")).map_err(err_lambda)?;
    copy_recurse(&refind_files.join("drivers_x64"), &refind_dir.join("drivers")).map_err(err_lambda)?;
    copy_recurse(&refind_files.join("tools_x64"), &mount.join("EFI/tools")).map_err(err_lambda)?;
    copy(refind_files.join("refind_x64.efi"), refind_dir.join("BOOTX64.EFI")).map_err(err_lambda)?;
    Ok(())
}

fn write_refind_config(config: &str, esp: &Path) -> Result<(),String> {
    println!("Writing rEFInd config");
    write(esp.join("EFI/BOOT/refind.conf"), config.as_bytes()).map_err(|e| format!("Could not write refind config: {}", e))
}

fn copy_partitions(isos: &[&Path], device: &Path) -> Result<String, String> {
    let mut config: String = String::new();
    for iso in isos {
        let iso_parts = get_device_parts(iso).map_err(|e| format!("Could not read partitions of disk image file {}: {}", iso.display(), e))?;
        let disk_parts = iso_parts.iter().map(iso_part_to_disk).collect::<Vec<Part>>();
        let bootable_parts_disk = disk_parts.iter().filter_map(|p| if p.type_uuid == ESP_UUID {Some(p.part_uuid)} else {None}).collect::<Vec<Uuid>>();
        let mut partn_iso_to_disk = HashMap::<u8,Uuid>::new();
        partn_iso_to_disk.extend(iso_parts.iter().map(|p| p.number).zip(disk_parts.iter().map(|p| p.part_uuid)));
        assert_eq!(iso_parts.len(), disk_parts.len());
        assert_eq!(iso_parts.len(), partn_iso_to_disk.len());
        println!("Creating partitions on destination device");
        run_sfd_script(false, device, &parts_to_sfd_script(&disk_parts))?;
        // Re-scan the disk to get new headers
        let created_disk_parts = get_device_parts(device).map_err(|e| format!("Could not read partition of disk {}: {}", device.display(), e))?;
        let mut iso_partn_to_created = HashMap::new();
        for (iso_partn, disk_guid) in partn_iso_to_disk {
            iso_partn_to_created.insert(iso_partn, created_disk_parts.iter().find(|p| p.part_uuid == disk_guid).ok_or(format!("sfdisk did not create any partition with guid {}", disk_guid))?);
        }
        assert_eq!(iso_parts.len(), iso_partn_to_created.len());
        for p in iso_parts {
            let created_part = iso_partn_to_created.get(&p.number).unwrap();
            if p.size > created_part.size {
                return Err(format!("Partition {} on ISO {} is larger than destination partition {}", p.number, p.part_uuid, created_part.part_uuid))
            }
            println!("Copying ISO {} partition {} to device partition {}", iso.display(), p.number, created_part.number);
            dd_copy_data(iso, device, p.start, created_part.start, p.size)?;
        }
        for boot_uuid in bootable_parts_disk {
            config.push_str(&write_menuentry(&iso.file_name().map_or("ISO".to_owned(), |s| s.display().to_string()), &boot_uuid));
        }
    }
    config.push_str("\ntimeout 30\nscanfor manual\n");
    Ok(config)
}

fn panic_with_msg<T: Display, S>(t: T) -> S {
    panic!("{}", t);
}

fn main() {
    let mut args: Vec<String> = env::args().collect();

    /*// Partition scan test
    println!("{:?}", get_device_parts(args[1].as_ref()));
    return;*/

    if args[1] == "--help" || args.len() < 4 {
        println!("multiiso BLOCK_DEVICE REFIND_FOLDER ISO...");
        println!("BLOCK_DEVICE is the device to use multiiso on, usually  of the form /dev/sd*");
        println!("REFIND_FOLDER is the folder \"refind\" from the refind zip file, avaliable at https://www.rodsbooks.com/refind/getting.html");
        println!("ISO... are ISO files which you wish to put onto the drive. They must be suitable for use with dd and EFI.");
        return;
    }

    let iso_list: Vec<String> = args.split_off(3);
    let block_dev = args[1].clone();
    let refind_dir = args[2].clone();
    let mount_for_refind = temp_dir().join("multiiso");
    create_dir_all(mount_for_refind.clone()).unwrap_or_else(panic_with_msg);
    install_mount_refind(block_dev.as_ref(), mount_for_refind.as_ref(), refind_dir.as_ref()).unwrap_or_else(panic_with_msg);
    let config = copy_partitions(&iso_list.iter().map(|x| <str as AsRef<Path>>::as_ref(x)).collect::<Vec<&Path>>(), block_dev.as_ref()).unwrap_or_else(panic_with_msg);
    //rescan_device(block_dev.as_ref()).unwrap_or_else(panic_with_msg);
    write_refind_config(&config, mount_for_refind.as_ref()).unwrap_or_else(panic_with_msg);
    run_cmd("umount", false, &[mount_for_refind]).unwrap_or_else(panic_with_msg);
    rescan_device(&block_dev.as_ref()).unwrap_or_else(panic_with_msg);

    //println!("{}", write_menuentry("ubuntu 24", Uuid::new_v4()));
    /*let mut line = String::new();
    stdin().read_line(&mut line).unwrap();
    println!("{}", parts_to_sfd_script(&read_parted_json(&line).unwrap().iter().map(iso_part_to_disk).collect::<Vec<Part>>()));*/
}
