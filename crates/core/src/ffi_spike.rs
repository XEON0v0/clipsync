#[uniffi::export(callback_interface)]
pub trait FfiSpikeCallback: Send + Sync {
    fn receive(&self, message: String);
}

#[uniffi::export]
pub fn ffi_spike_round_trip(
    callback: Box<dyn FfiSpikeCallback>,
    message: String,
) -> String {
    callback.receive(message.clone());
    message
}
