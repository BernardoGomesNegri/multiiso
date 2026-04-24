# multiiso: a tool to boot more than one operating system in a single flash drive
Operating System distributors often ship their installers in the form of a disk image, an ISO file, that is meant to be recorded directly onto a flash drive (also called a thumb drive, or a pen drive). This often means that there is no easy way to put more than one disk image into a single thumb drive. This might have been acceptable when an Operating System installer filled multiple CDs, but in the age of online installers and 16 GB portable drives, it doesn't make sense that we should be restricted to only one image per drive.

multiiso is a Linux tool that solves the problem of only being able to write one disk image per flash drive. Using modern techniques for managing boot, it can put more than one disk image into a flash drive, without compromise. Using Roderick W. Smith's [rEFInd](https://www.rodsbooks.com/refind/) it displays a user-friendly graphical screen to select which disk image should be used to boot. Additionally, using rEFInd means access to tools such as an EFI shell if installed, which can make it easier for power users.

## Requirements
In addition to cargo and a Rust installation for compiling, the following programs are a runtime requirement for multiiso:

- `mkfs.fat`
- `udevadm`
- `blockdev`
- `mount`
- `sfdisk`
- The `refind` folder from rEFInd, which can be obtained from the [rEFInd's binary zip file](https://www.rodsbooks.com/refind/getting.html). The folder must contain the `refind_x64.efi` file as well as the `icons`, `drivers_x64` and `tools_x64` folders.

For the last requirement, you will need to point to the `refind` folder in the command line. You don't need to actually install rEFInd on your system.

## Executing
Run the binary `multiiso`. The first argument is the block device on which you wish to record the ISO files. Note that **all data on that device will be wiped with NO WARNING**. This is usually `/dev/sdb` or `/dev/sdc`. The second argument is the `refind` folder from the rEFInd distribution. The folder must contain the `refind_x64.efi` file as well as the `icons`, `drivers_x64` and `tools_x64` folders. Then all other arguments should be paths to the ISO files you wish to record in the flash drive. Note that some ISO files may not work due to having strange partition layouts.

Example:

```
$ ls refind
tools_x64  icons  drivers_x64  refind_x64.efi
$ sudo multiiso /dev/sdb refind/ Fedora.iso OpenSuse.iso
```

## Compiling
Just a standard Rust build. Just run `cargo build`.

## Technical details and caveats
The [EFI specification](https://uefi.org/specs/UEFI/2.10/13_Protocols_Media_Access.html#partition-discovery) defines three types of partition discovery methods in bootable removable media: [Master Boot Record (MBR) partition table](https://en.wikipedia.org/wiki/Master_boot_record), [GUID Partition Table (GPT)](https://en.wikipedia.org/wiki/GUID_Partition_Table) and [El Torito ISO 9660](https://en.wikipedia.org/wiki/ISO_9660#El_Torito). The latter is where the ".iso" extension comes from, and is meant for DVDs and CDS. However, most disk images you download from OS vendors will contain both an El Torito table and a GPT, because they want the file to be bootable both if it is recorded on a CD/DVD and if it is booted from a flash drive. No matter the partition discovery method, the contents of a disk image will usually be the same: one EFI system partition formatted as FAT, and other auxiliary partitions which may be in any format (but are typically ISO9660, to preserve compatibility with DVDs and CDs).

However, this method may not work if it is not possible to read the partition table in the disk image. Currently, some ISOHybrid disk images (usually created by Debian-based distributions) do not work because the GPT in those files is incorrect (they contain overlapping partitions). Because this method puts multiple partitions in one disk image, there may be issues when using multiple similar Linux distributions, as they can't differentiate between their auxiliary partition and a similar distribution's auxiliary partition.

How multiiso works is that it copies the EFI system partition and auxiliary partitions from every ISO file to the flash drive, then setups rEFInd as a menu to allow you to boot into any of the other EFI partitions. This method can be more reliable than ventoy, as it does not rely on creating a virtual EFI device or accessing the Linux device mapper after boot but before the OS is loaded. However, strangely formatted disk images can cause the process to fail.
