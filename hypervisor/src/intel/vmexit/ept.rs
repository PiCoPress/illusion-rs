use {
    crate::{
        error::HypervisorError,
        intel::{
            invept::invept_all_contexts, support::vmread, support::vmwrite, vm::Vm,
            vmerror::EptViolationExitQualification, vmexit::ExitType,
        },
    },
    log::*,
    x86::vmx::vmcs,
};

// Define an enum to represent EPTP states
#[derive(Debug, Clone, Copy)]
pub enum EptpState {
    Primary,
    Secondary,
}

/// Handle VM exits for EPT violations. Violations are thrown whenever an operation is performed on an EPT entry that does not provide permissions to access that page.
/// 29.3.3.2 EPT Violations
/// Table 28-7. Exit Qualification for EPT Violations
#[rustfmt::skip]
pub fn handle_ept_violation(vm: &mut Vm) -> ExitType {
    debug!("Handling EPT Violation VM exit...");

    let guest_physical_address = vmread(vmcs::ro::GUEST_PHYSICAL_ADDR_FULL);
    debug!("EPT Violation: Guest Physical Address: {:#x}", guest_physical_address);

    // dump_primary_and_secondary_ept(vm, guest_physical_address);

    let exit_qualification_value = vmread(vmcs::ro::EXIT_QUALIFICATION);
    let ept_violation_qualification = EptViolationExitQualification::from_exit_qualification(exit_qualification_value);
    debug!("Exit Qualification for EPT Violations: {:#?}", ept_violation_qualification);

    // Handle the EPT violation based on the instruction fetch status and page permissions for memory introspection.
    // An EPT Violation vmexit occurs when a guest attempts to access a page that its current EPT settings do not permit.
    // Depending on the type of violation, we switch the EPTP to either the primary or secondary EPTP:
    // - If the guest attempts to execute from a page that is only permitted for reading and writing, we switch to the secondary EPTP.
    // - If the guest attempts to read or write to a page that is only executable, we switch to the primary EPTP.
    // This approach ensures the host's code remains hidden from the guest, preventing improper access to host memory.
    match ept_violation_qualification.instruction_fetch {
        // When instruction fetch is false, it indicates an attempt to read or write to a page marked as Execute-Only.
        // Here, we:
        // - Switch to the primary EPTP,
        // - Invalidate the EPT cache to refresh permissions, and
        // - This typically involves a page where:
        //   Instruction Fetch: false,
        //   Page Permissions: R:false, W:false, X:true (non-readable, non-writable, but executable).
        false => {
            switch_eptp(vm, EptpState::Primary);
        },
        // When instruction fetch is true, it signifies an attempt to execute code from a page marked as Read-Write-Only.
        // Actions taken include:
        // - Switching to the secondary EPTP,
        // - Invalidating the EPT cache, and
        // - This typically involves a page where:
        //   Instruction Fetch: true,
        //   Page Permissions: R:true, W:true, X:false (readable, writable, but non-executable).
        true => {
            switch_eptp(vm, EptpState::Secondary);
        },
    };

    debug!("EPT Violation handled successfully!");

    // Do not increment RIP, since we want it to execute the same instruction again.
    ExitType::Continue
}

/// Attempts to switch the EPTP state and logs appropriate messages.
pub fn switch_eptp(vm: &mut Vm, state: EptpState) {
    let eptp_address = match state {
        EptpState::Primary => unsafe { vm.shared_data.as_ref().primary_eptp },
        EptpState::Secondary => unsafe { vm.shared_data.as_ref().secondary_eptp },
    };

    // Switch to the new EPTP address.
    vmwrite(vmcs::control::EPTP_FULL, eptp_address);

    // Invalidate EPT caches.
    invept_all_contexts();

    trace!(
        "Switched to {:?} EPTP at address: {:#x}",
        state,
        eptp_address
    );
}

/// Handles an EPT misconfiguration VM exit.
///
/// This function is invoked when an EPT misconfiguration VM exit occurs, indicating
/// an issue with the Extended Page Tables (EPT) setup. It logs the faulting
/// guest physical address and triggers a breakpoint exception for immediate debugging.
///
/// # Safety
///
/// This function executes an `int3` instruction, which triggers a breakpoint exception.
/// This is used for debugging critical issues and should be employed cautiously.
/// Appropriate debugging tools must be attached to handle the `int3` exception.
///
/// Note: EPT misconfigurations are critical errors that can lead to system instability or crashes.
/// Continuing normal execution after such an exception is not recommended, as it may result in
/// unpredictable behavior or a crashed operating system.
///
/// Reference: 29.3.3.1 EPT Misconfigurations
#[rustfmt::skip]
pub fn handle_ept_misconfiguration(vm: &mut Vm) -> ExitType {
    debug!("Handling EPT Misconfiguration VM exit...");

    // Retrieve the guest physical address that caused the EPT misconfiguration.
    let guest_physical_address = vmread(vmcs::ro::GUEST_PHYSICAL_ADDR_FULL);

    trace!("EPT Misconfiguration: Faulting guest address: {:#x}. This is a critical error that cannot be safely ignored.", guest_physical_address);
    dump_primary_and_secondary_ept(vm, guest_physical_address);

    // Trigger a breakpoint exception to halt execution for debugging.
    // Continuing after this point is unsafe due to the potential for system instability.
    unsafe {  core::arch::asm!("int3") };

    // Execution should not continue beyond this point.
    // EPT misconfiguration is a fatal exception and continuing may lead to system crashes.

    // We may chose to exit the hypervisor here instead of triggering a breakpoint exception.
    return ExitType::ExitHypervisor;
}

/// Dumps the EPT entries for the primary and secondary EPTs at the specified guest physical address.
///
/// This function is used for debugging EPT misconfigurations and violations and prints the EPT entries for the primary and secondary EPTs.
///
/// # Arguments
///
/// * `vm` - The virtual machine instance.
/// * `guest_physical_address` - The guest physical address that caused the EPT misconfiguration or violation.
pub fn dump_primary_and_secondary_ept(vm: &mut Vm, guest_physical_address: u64) {
    // Log the critical error information.
    trace!("Faulting guest address: {:#x}", guest_physical_address);

    // Get the shared data from the VM.
    let shared_data = unsafe { vm.shared_data.as_mut() };

    // We minus 1 from the current hook index to get the current hook index because the current hook index is incremented when the hook was placed in `ept_hook`.
    trace!(
        "Current Hook Index: {:#x}",
        shared_data.current_hook_index - 1
    );

    // Access the current hook based on `current_hook_index`
    let hook = shared_data
        .ept_hook_manager
        .get_mut(shared_data.current_hook_index - 1)
        .ok_or(HypervisorError::FailedToGetCurrentHookIndex)
        .unwrap();

    //trace!("Hook Index {:#x}", shared_data.current_hook_index);

    // Get the primary and secondary EPTs.
    let primary_ept = &mut shared_data.primary_ept;
    let secondary_ept = &mut shared_data.secondary_ept;

    trace!(
        "Dumping Primary EPT entries for guest physical address: {:#x}",
        guest_physical_address
    );
    primary_ept.dump_ept_entries(
        guest_physical_address,
        hook.primary_ept_pre_alloc_pt.as_mut(),
    );

    trace!(
        "Dumping Secondary EPT entries for guest physical address: {:#x}",
        guest_physical_address
    );
    secondary_ept.dump_ept_entries(
        guest_physical_address,
        hook.secondary_ept_pre_alloc_pt.as_mut(),
    );
}
