//! This module provides the functionality to locate the System Service Descriptor Table (SSDT)
//! within the Windows kernel. The SSDT is critical for intercepting system calls, allowing for
//! both monitoring and modification. This capability is particularly useful in the development
//! of security tools, hypervisors, and other low-level system utilities. By identifying the
//! addresses of the NT and Win32k tables, this module enables further manipulation of system
//! behavior at a granular level.
//!

use {crate::error::HypervisorError, alloc::vec::Vec};

/// Represents the addresses of the SSDT tables for NT and Win32k system calls.
pub struct SsdtFind {
    /// The address of the NT table within the SSDT.
    pub nt_table: *const u64,

    /// The address of the Win32k table within the SSDT.
    pub win32k_table: *const u64,
}

impl SsdtFind {
    /// Locates the SSDT based on a given kernel base and size.
    ///
    /// This function scans the kernel memory for specific patterns to find the SSDT addresses.
    /// It leverages known structures within the Windows kernel to pinpoint the exact location
    /// of the NT and Win32k service descriptor tables.
    ///
    /// # Arguments
    ///
    /// * `kernel_base` - The base address of the kernel in memory.
    /// * `kernel_size` - The size of the kernel memory space.
    ///
    /// # Returns
    ///
    /// * `Ok(SsdtFind)` - An `SsdtFind` struct containing the addresses of the NT and Win32k tables.
    /// * `Err(HypervisorError)` - An error occurred during the scanning process.
    pub fn find_ssdt(kernel_base: *const u8, kernel_size: usize) -> Result<Self, HypervisorError> {
        log::debug!("Kernel base address: {:p}", kernel_base);
        log::debug!("Kernel size: {}", kernel_size);

        /*
           14042ba50  uint64_t KiSystemServiceStart(int64_t arg1, int64_t arg2, uint64_t arg3, int64_t arg4, int32_t arg5 @ rax, uint64_t arg6 @ rbx, int128_t* arg7 @ rbp, uint64_t arg8 @ ssp)

           14042ba50  4889a390000000     mov     qword [rbx+0x90], rsp {__return_addr}
           14042ba57  8bf8               mov     edi, eax
           14042ba59  c1ef07             shr     edi, 0x7
           14042ba5c  83e720             and     edi, 0x20
           14042ba5f  25ff0f0000         and     eax, 0xfff

           14042ba64  4c8d15555e9d00     lea     r10, [rel KeServiceDescriptorTable]
           14042ba6b  4c8d1d8e368f00     lea     r11, [rel KeServiceDescriptorTableShadow]
        */

        // Pattern to identify the KiServiceSystemStart in the kernel memory.
        let ki_service_system_start_pattern = "8B F8 C1 EF 07 83 E7 20 25 FF 0F 00 00";
        let signature_size = 13;

        // Create a slice from the Windows kernel (ntoskrnl.exe) base address for the specified size.
        let ntoskrnl_data = unsafe { core::slice::from_raw_parts(kernel_base, kernel_size) };

        // Find the starting offset of the KiServiceSystemStart pattern within the kernel data.
        let offset = Self::pattern_scan(ntoskrnl_data, ki_service_system_start_pattern)?
            .ok_or(HypervisorError::PatternNotFound)?;

        // Calculate the starting address of KiServiceSystemStart based on the offset.
        // That is: `14042ba57  8bf8               mov     edi, eax` in this case.
        let ki_service_system_start = unsafe { kernel_base.add(offset) };
        log::info!(
            "KiServiceSystemStart address: {:p}",
            ki_service_system_start
        );

        // Address of the 'lea r10, [rel KeServiceDescriptorTable]' instruction
        let lea_r10_address = unsafe { ki_service_system_start.add(signature_size) };

        // Address of the 'lea r11, [rel KeServiceDescriptorTableShadow]' instruction
        let lea_r11_address = unsafe { lea_r10_address.add(7) }; // 7 bytes after lea r10

        // Reading the 4-byte relative offset for KeServiceDescriptorTableShadow
        let relative_offset = unsafe { *(lea_r11_address.add(3) as *const i32) }; // 3 bytes after the opcode

        log::info!("Relative offset: {:x}", relative_offset);

        // Compute the absolute address of KeServiceDescriptorTableShadow
        let ke_service_descriptor_table_shadow =
            unsafe { lea_r11_address.add(7).offset(relative_offset as isize) };

        // Extracting nt!KiServiceTable and win32k!W32pServiceTable addresses
        let shadow = ke_service_descriptor_table_shadow;

        // NtTable Address of Nt Syscall Table
        let nt_table = shadow as *const u64;

        // Win32kTable Address of Win32k Syscall Table
        let win32k_table = unsafe { shadow.offset(0x20) as *const u64 };

        log::info!("NtTable address: {:p}", nt_table);
        log::info!("Win32kTable address: {:p}", win32k_table);

        Ok(Self {
            nt_table,
            win32k_table,
        })
    }

    /// Converts a pattern string into a vector of bytes, supporting wildcards.
    ///
    /// # Arguments
    ///
    /// * `pattern` - A string representing the byte pattern to convert, with spaces separating bytes.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Option<u8>>)` - A vector where each element represents a byte from the pattern. None values represent wildcards where any byte is acceptable.
    /// * `Err(HypervisorError)` - An error occurred during the conversion, likely due to an invalid hex value.
    pub fn get_bytes_as_hex(pattern: &str) -> Result<Vec<Option<u8>>, HypervisorError> {
        let mut pattern_bytes = Vec::new();

        for x in pattern.split_whitespace() {
            match x {
                "?" => pattern_bytes.push(None),
                _ => pattern_bytes.push(
                    u8::from_str_radix(x, 16)
                        .map(Some)
                        .map_err(|_| HypervisorError::HexParseError)?,
                ),
            }
        }

        Ok(pattern_bytes)
    }

    /// Searches for a given pattern within a block of data and returns the start index if found.
    ///
    /// This function implements a simple pattern matching algorithm to scan a region of memory
    /// for a specific byte pattern, supporting wildcards. It's useful for locating specific
    /// instructions or data structures within a binary blob by their binary signatures.
    ///
    /// # Arguments
    ///
    /// * `data` - The block of data to search within.
    /// * `pattern` - The byte pattern to search for, expressed as a space-separated string of hex values.
    ///   Wildcards are represented by "?".
    ///
    /// # Returns
    ///
    /// * `Ok(Some(usize))` - The starting index within `data` where the pattern was found.
    /// * `Ok(None)` - The pattern was not found within `data`.
    /// * `Err(HypervisorError)` - An error occurred during pattern conversion or search.
    pub fn pattern_scan(data: &[u8], pattern: &str) -> Result<Option<usize>, HypervisorError> {
        let pattern_bytes = Self::get_bytes_as_hex(pattern)?;

        // Iterate over the data in windows of size equal to the pattern length,
        // checking if each window matches the pattern.
        let offset = data.windows(pattern_bytes.len()).position(|window| {
            window
                .iter()
                .zip(&pattern_bytes)
                .all(|(byte, pattern_byte)| pattern_byte.map_or(true, |b| *byte == b))
        });

        Ok(offset)
    }
}
