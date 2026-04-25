use crate::core::{error, RxStartMode};
use std::ops::Deref;

const DEFAULT_RX_BUFFER_LEN: usize = 65_535;
const DEFAULT_RX_BUFFER_COUNT: usize = 1_024;
const DEFAULT_RX_RING_ENTRIES: u32 = 256;
const DEFAULT_TX_RING_ENTRIES: u32 = 256;
const DEFAULT_RX_AUTO_RESUME_AFTER_RECYCLED_SLOTS: usize = 0;
const DEFAULT_TX_SUBMIT_CHUNK_SIZE: usize = 64;

#[derive(Clone, Debug)]
pub struct UringDeviceConfig {
    pub rx_buffer_len: usize,
    pub rx_buffer_count: usize,
    pub rx_ring_entries: u32,
    pub tx_ring_entries: u32,
    pub rx_auto_resume_after_recycled_slots: usize,
    pub rx_start_mode: RxStartMode,
    pub tx_submit_chunk_size: usize,
}

impl Default for UringDeviceConfig {
    fn default() -> Self {
        Self {
            rx_buffer_len: DEFAULT_RX_BUFFER_LEN,
            rx_buffer_count: DEFAULT_RX_BUFFER_COUNT,
            rx_ring_entries: DEFAULT_RX_RING_ENTRIES,
            tx_ring_entries: DEFAULT_TX_RING_ENTRIES,
            rx_auto_resume_after_recycled_slots: DEFAULT_RX_AUTO_RESUME_AFTER_RECYCLED_SLOTS,
            rx_start_mode: RxStartMode::AutoStart,
            tx_submit_chunk_size: DEFAULT_TX_SUBMIT_CHUNK_SIZE,
        }
    }
}

impl UringDeviceConfig {
    #[must_use]
    pub fn with_rx_buffer_len(mut self, value: usize) -> Self {
        self.rx_buffer_len = value;
        self
    }

    #[must_use]
    pub fn with_rx_buffer_count(mut self, value: usize) -> Self {
        self.rx_buffer_count = value;
        self
    }

    #[must_use]
    pub fn with_rx_ring_entries(mut self, value: u32) -> Self {
        self.rx_ring_entries = value;
        self
    }

    #[must_use]
    pub fn with_tx_ring_entries(mut self, value: u32) -> Self {
        self.tx_ring_entries = value;
        self
    }

    #[must_use]
    pub fn with_rx_auto_resume_after_recycled_slots(mut self, value: usize) -> Self {
        self.rx_auto_resume_after_recycled_slots = value;
        self
    }

    #[must_use]
    pub fn with_rx_start_mode(mut self, value: RxStartMode) -> Self {
        self.rx_start_mode = value;
        self
    }

    #[must_use]
    pub fn with_tx_submit_chunk_size(mut self, value: usize) -> Self {
        self.tx_submit_chunk_size = value;
        self
    }

    #[allow(dead_code)]
    fn validate(self) -> std::io::Result<ValidatedConfig> {
        if self.rx_buffer_len == 0 {
            return Err(error::invalid_config(
                "rx_buffer_len",
                "must be greater than 0",
            ));
        }

        if self.rx_buffer_count == 0 {
            return Err(error::invalid_config(
                "rx_buffer_count",
                "must be greater than 0",
            ));
        }

        if !self.rx_buffer_count.is_power_of_two() {
            return Err(error::invalid_config(
                "rx_buffer_count",
                "must be a power of 2 for provided buffer rings",
            ));
        }

        if self.rx_buffer_count > 32_768 {
            return Err(error::invalid_config(
                "rx_buffer_count",
                "must not exceed 32768 for provided buffer rings",
            ));
        }

        if self.rx_buffer_len > u32::MAX as usize {
            return Err(error::invalid_config(
                "rx_buffer_len",
                "must fit within u32 for io_uring read_multishot",
            ));
        }

        if self.rx_ring_entries == 0 {
            return Err(error::invalid_config(
                "rx_ring_entries",
                "must be greater than 0",
            ));
        }

        if self.tx_ring_entries == 0 {
            return Err(error::invalid_config(
                "tx_ring_entries",
                "must be greater than 0",
            ));
        }

        if self.tx_submit_chunk_size == 0 {
            return Err(error::invalid_config(
                "tx_submit_chunk_size",
                "must be greater than 0",
            ));
        }

        Ok(ValidatedConfig(self))
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ValidatedConfig(UringDeviceConfig);

impl ValidatedConfig {
    pub(crate) fn to_config(&self) -> UringDeviceConfig {
        self.0.clone()
    }
}

impl TryFrom<UringDeviceConfig> for ValidatedConfig {
    type Error = std::io::Error;

    fn try_from(value: UringDeviceConfig) -> Result<Self, Self::Error> {
        value.validate()
    }
}

impl Deref for ValidatedConfig {
    type Target = UringDeviceConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{UringDeviceConfig, ValidatedConfig};

    #[test]
    fn rejects_zero_rx_buffer_len() {
        let error = ValidatedConfig::try_from(UringDeviceConfig::default().with_rx_buffer_len(0))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.rx_buffer_len"));
    }

    #[test]
    fn rejects_zero_rx_buffer_count() {
        let error = ValidatedConfig::try_from(UringDeviceConfig::default().with_rx_buffer_count(0))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.rx_buffer_count"));
    }

    #[test]
    fn rejects_non_power_of_two_rx_buffer_count() {
        let error = ValidatedConfig::try_from(UringDeviceConfig::default().with_rx_buffer_count(3))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.rx_buffer_count"));
    }

    #[test]
    fn rejects_too_large_rx_buffer_count() {
        let error =
            ValidatedConfig::try_from(UringDeviceConfig::default().with_rx_buffer_count(65_536))
                .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.rx_buffer_count"));
    }

    #[test]
    fn rejects_zero_rx_ring_entries() {
        let error = ValidatedConfig::try_from(UringDeviceConfig::default().with_rx_ring_entries(0))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.rx_ring_entries"));
    }

    #[test]
    fn rejects_zero_tx_ring_entries() {
        let error = ValidatedConfig::try_from(UringDeviceConfig::default().with_tx_ring_entries(0))
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.tx_ring_entries"));
    }

    #[test]
    fn rejects_zero_tx_submit_chunk_size() {
        let error =
            ValidatedConfig::try_from(UringDeviceConfig::default().with_tx_submit_chunk_size(0))
                .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("config.tx_submit_chunk_size"));
    }

    #[test]
    fn accepts_default_configuration() {
        let validated = ValidatedConfig::try_from(UringDeviceConfig::default()).unwrap();

        assert_eq!(validated.rx_buffer_len, 65_535);
        assert_eq!(validated.rx_buffer_count, 1_024);
        assert_eq!(validated.rx_ring_entries, 256);
        assert_eq!(validated.tx_ring_entries, 256);
        assert_eq!(validated.tx_submit_chunk_size, 64);
    }
}
