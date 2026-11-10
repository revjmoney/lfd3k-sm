//! SX1262 command opcodes, register addresses, IRQ bits and encoders.
//!
//! Every encoder returns a fixed array holding the full SPI frame
//! (opcode first, then parameters, most significant byte first).

// Opcodes (SX1261/2 datasheet, chapter 13).
pub const SET_STANDBY: u8 = 0x80;
pub const SET_PACKET_TYPE: u8 = 0x8A;
pub const SET_RF_FREQUENCY: u8 = 0x86;
pub const SET_PA_CONFIG: u8 = 0x95;
pub const SET_TX_PARAMS: u8 = 0x8E;
pub const SET_MODULATION_PARAMS: u8 = 0x8B;
pub const SET_PACKET_PARAMS: u8 = 0x8C;
pub const SET_DIO_IRQ_PARAMS: u8 = 0x08;
pub const GET_IRQ_STATUS: u8 = 0x12;
pub const CLEAR_IRQ_STATUS: u8 = 0x02;
pub const SET_DIO2_AS_RF_SWITCH_CTRL: u8 = 0x9D;
pub const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
pub const CALIBRATE: u8 = 0x89;
pub const CALIBRATE_IMAGE: u8 = 0x98;
pub const SET_REGULATOR_MODE: u8 = 0x96;
pub const SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
pub const WRITE_BUFFER: u8 = 0x0E;
pub const READ_BUFFER: u8 = 0x1E;
pub const WRITE_REGISTER: u8 = 0x0D;
pub const READ_REGISTER: u8 = 0x1D;
pub const SET_TX: u8 = 0x83;
pub const SET_RX: u8 = 0x82;
pub const GET_RX_BUFFER_STATUS: u8 = 0x13;
pub const GET_PACKET_STATUS: u8 = 0x14;
pub const GET_STATUS: u8 = 0xC0;
pub const CLEAR_DEVICE_ERRORS: u8 = 0x07;
pub const SET_RX_TX_FALLBACK_MODE: u8 = 0x93;

/// Filler byte clocked out while reading.
pub const NOP: u8 = 0x00;

// IRQ bits.
pub const IRQ_TX_DONE: u16 = 1 << 0;
pub const IRQ_RX_DONE: u16 = 1 << 1;
pub const IRQ_PREAMBLE_DETECTED: u16 = 1 << 2;
pub const IRQ_HEADER_VALID: u16 = 1 << 4;
pub const IRQ_HEADER_ERR: u16 = 1 << 5;
pub const IRQ_CRC_ERR: u16 = 1 << 6;
pub const IRQ_TIMEOUT: u16 = 1 << 9;
/// Every IRQ the driver uses; all are routed to DIO1.
pub const IRQ_USED: u16 = IRQ_TX_DONE
    | IRQ_RX_DONE
    | IRQ_PREAMBLE_DETECTED
    | IRQ_HEADER_VALID
    | IRQ_HEADER_ERR
    | IRQ_CRC_ERR
    | IRQ_TIMEOUT;
const _: () = assert!(IRQ_USED == 0x0277);
/// All IRQ bits, for clearing everything.
pub const IRQ_ALL: u16 = 0x03FF;

// Registers.
pub const REG_SYNC_WORD_MSB: u16 = 0x0740;
pub const REG_SYNC_WORD_LSB: u16 = 0x0741;
pub const REG_SENSITIVITY: u16 = 0x0889; // errata 15.1
pub const REG_TX_CLAMP: u16 = 0x08D8; // errata 15.2
pub const REG_OCP: u16 = 0x08E7;

// Parameter values.
pub const STDBY_RC: u8 = 0x00;
pub const STDBY_XOSC: u8 = 0x01;
pub const PACKET_TYPE_LORA: u8 = 0x01;
pub const TCXO_1V8: u8 = 0x02;
/// TCXO start delay: 5 ms in 15.625 us steps.
pub const TCXO_DELAY_STEPS: u32 = 320;
pub const CALIBRATE_ALL: u8 = 0x7F;
pub const REGULATOR_DC_DC: u8 = 0x01;
/// CalibrateImage band 902 to 928 MHz.
pub const IMAGE_902_928: [u8; 2] = [0xE1, 0xE9];
/// SetPaConfig for SX1262 high power PA: duty 0x04, hpMax 0x07, device 0, lut 1.
pub const PA_CONFIG: [u8; 4] = [0x04, 0x07, 0x00, 0x01];
pub const OCP_140MA: u8 = 0x38;
pub const RAMP_40US: u8 = 0x02;
pub const SYNC_WORD: [u8; 2] = [0x14, 0x24];
pub const FALLBACK_STDBY_XOSC: u8 = 0x30;
pub const BW_500: u8 = 0x06;
pub const CR_4_5: u8 = 0x01;
pub const LDRO_OFF: u8 = 0x00;
pub const HEADER_EXPLICIT: u8 = 0x00;
pub const CRC_ON: u8 = 0x01;
pub const IQ_STANDARD: u8 = 0x00;
/// SetRx timeout for continuous receive.
pub const RX_CONTINUOUS: u32 = 0x00FF_FFFF;
/// SetTx timeout 0: no timeout.
pub const TX_NO_TIMEOUT: u32 = 0;

/// Chip mode field (bits 6:4) of the status byte.
pub const fn chip_mode(status: u8) -> u8 {
    (status >> 4) & 0x07
}
pub const MODE_STDBY_RC: u8 = 0x2;
pub const MODE_TX: u8 = 0x6;

/// SX1262 frequency word: `hz * 2^25 / 32 MHz`, integer math.
pub const fn freq_word(hz: u32) -> u32 {
    ((hz as u64) * (1u64 << 25) / 32_000_000u64) as u32
}
const _: () = assert!(freq_word(lfd3k_core::CHANNEL_HZ) == 0x3968_0000);

const fn be24(v: u32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

pub const fn set_standby(mode: u8) -> [u8; 2] {
    [SET_STANDBY, mode]
}

pub const fn set_packet_type(t: u8) -> [u8; 2] {
    [SET_PACKET_TYPE, t]
}

pub const fn set_rf_frequency(hz: u32) -> [u8; 5] {
    let w = freq_word(hz);
    [
        SET_RF_FREQUENCY,
        (w >> 24) as u8,
        (w >> 16) as u8,
        (w >> 8) as u8,
        w as u8,
    ]
}

pub const fn set_pa_config(p: [u8; 4]) -> [u8; 5] {
    [SET_PA_CONFIG, p[0], p[1], p[2], p[3]]
}

/// Power in dBm (two's complement byte) and ramp code.
pub const fn set_tx_params(power_dbm: i8, ramp: u8) -> [u8; 3] {
    [SET_TX_PARAMS, power_dbm as u8, ramp]
}

pub const fn set_modulation_params(sf: u8, bw: u8, cr: u8, ldro: u8) -> [u8; 5] {
    [SET_MODULATION_PARAMS, sf, bw, cr, ldro]
}

pub const fn set_packet_params(preamble: u16, header: u8, len: u8, crc: u8, iq: u8) -> [u8; 7] {
    [
        SET_PACKET_PARAMS,
        (preamble >> 8) as u8,
        preamble as u8,
        header,
        len,
        crc,
        iq,
    ]
}

/// IRQ mask, then DIO1, DIO2 and DIO3 masks.
pub const fn set_dio_irq_params(irq: u16, dio1: u16, dio2: u16, dio3: u16) -> [u8; 9] {
    [
        SET_DIO_IRQ_PARAMS,
        (irq >> 8) as u8,
        irq as u8,
        (dio1 >> 8) as u8,
        dio1 as u8,
        (dio2 >> 8) as u8,
        dio2 as u8,
        (dio3 >> 8) as u8,
        dio3 as u8,
    ]
}

/// Frame for GetIrqStatus: reply is `[_, status, irq_msb, irq_lsb]`.
pub const fn get_irq_status() -> [u8; 4] {
    [GET_IRQ_STATUS, NOP, NOP, NOP]
}

pub const fn clear_irq_status(mask: u16) -> [u8; 3] {
    [CLEAR_IRQ_STATUS, (mask >> 8) as u8, mask as u8]
}

pub const fn set_dio2_as_rf_switch(on: bool) -> [u8; 2] {
    [SET_DIO2_AS_RF_SWITCH_CTRL, on as u8]
}

pub const fn set_dio3_as_tcxo(voltage: u8, delay_steps: u32) -> [u8; 5] {
    let d = be24(delay_steps);
    [SET_DIO3_AS_TCXO_CTRL, voltage, d[0], d[1], d[2]]
}

pub const fn calibrate(mask: u8) -> [u8; 2] {
    [CALIBRATE, mask]
}

pub const fn calibrate_image(band: [u8; 2]) -> [u8; 3] {
    [CALIBRATE_IMAGE, band[0], band[1]]
}

pub const fn set_regulator_mode(mode: u8) -> [u8; 2] {
    [SET_REGULATOR_MODE, mode]
}

pub const fn set_buffer_base_address(tx: u8, rx: u8) -> [u8; 3] {
    [SET_BUFFER_BASE_ADDRESS, tx, rx]
}

pub const fn set_tx(timeout: u32) -> [u8; 4] {
    let t = be24(timeout);
    [SET_TX, t[0], t[1], t[2]]
}

pub const fn set_rx(timeout: u32) -> [u8; 4] {
    let t = be24(timeout);
    [SET_RX, t[0], t[1], t[2]]
}

/// Frame for GetRxBufferStatus: reply is `[_, status, len, start]`.
pub const fn get_rx_buffer_status() -> [u8; 4] {
    [GET_RX_BUFFER_STATUS, NOP, NOP, NOP]
}

/// Frame for GetPacketStatus: reply is `[_, status, rssi_pkt, snr_pkt, signal_rssi]`.
pub const fn get_packet_status() -> [u8; 5] {
    [GET_PACKET_STATUS, NOP, NOP, NOP, NOP]
}

/// Frame for GetStatus: reply is `[_, status]`.
pub const fn get_status() -> [u8; 2] {
    [GET_STATUS, NOP]
}

pub const fn clear_device_errors() -> [u8; 3] {
    [CLEAR_DEVICE_ERRORS, 0x00, 0x00]
}

pub const fn set_rx_tx_fallback_mode(mode: u8) -> [u8; 2] {
    [SET_RX_TX_FALLBACK_MODE, mode]
}

/// WriteRegister of one byte.
pub const fn write_register(addr: u16, value: u8) -> [u8; 4] {
    [WRITE_REGISTER, (addr >> 8) as u8, addr as u8, value]
}

/// ReadRegister of one byte: reply is `[_, _, _, status, value]`.
pub const fn read_register(addr: u16) -> [u8; 5] {
    [READ_REGISTER, (addr >> 8) as u8, addr as u8, NOP, NOP]
}

/// Header of WriteBuffer; the payload follows in the same transfer.
pub const fn write_buffer_header(offset: u8) -> [u8; 2] {
    [WRITE_BUFFER, offset]
}

/// Header of ReadBuffer (opcode, offset, NOP); data follows in the reply.
pub const fn read_buffer_header(offset: u8) -> [u8; 3] {
    [READ_BUFFER, offset, NOP]
}
