//! Thread QoS classes (`pthread/qos.h`): background work (ASR on import, mask tracks)
//! runs at utility so the UI and the render thread keep the performance cores.

/// `QOS_CLASS_UTILITY` and `QOS_CLASS_DEFAULT` (`sys/qos.h`).
const QOS_CLASS_UTILITY: u32 = 0x11;
const QOS_CLASS_DEFAULT: u32 = 0x15;

unsafe extern "C" {
    // pthread/qos.h (libSystem).
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// Sets the calling thread's QoS class to utility. Returns whether the OS accepted it.
pub fn set_utility() -> bool {
    // SAFETY: plain FFI with a valid class constant and priority 0; it only affects the
    // calling thread's scheduling.
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) == 0 }
}

/// Sets the calling thread's QoS class back to default. Returns whether the OS accepted it.
pub fn set_default() -> bool {
    // SAFETY: as above.
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) == 0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_thread_can_drop_to_utility_and_back() {
        assert!(
            std::thread::spawn(|| super::set_utility() && super::set_default())
                .join()
                .unwrap()
        );
    }
}
