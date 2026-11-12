/* Heltec pin maps. Default: the V4 (plain V4 OLED, GC1109 front end).
 * LFD3K_BOARD_V3 (set by the heltec_v3_single env): the WiFi LoRa 32 V3, the same
 * SX1262, OLED, Vext, LED and PRG pins, but no front end (the SX1262 drives
 * the antenna directly, so S30 is the dBm at the antenna, 22 at most) and a
 * CP2102 USB bridge on UART0 instead of native USB. */
#ifndef BOARD_H
#define BOARD_H

#ifdef LFD3K_BOARD_V3
#define BOARD_HAS_FEM  0
#define BOARD_NAME     "HELTEC V3"
#else
#define BOARD_HAS_FEM  1
#define BOARD_NAME     "HELTEC V4"
#endif

#define PIN_LORA_NSS   8
#define PIN_LORA_SCK   9
#define PIN_LORA_MOSI  10
#define PIN_LORA_MISO  11
#define PIN_LORA_RST   12
#define PIN_LORA_BUSY  13
#define PIN_LORA_DIO1  14

#if BOARD_HAS_FEM
/* GC1109 front end (V4 only: on the V3 these pins are free for other uses,
 * so nothing here may touch them). */
#define PIN_FEM_POWER  7   /* LDO, HIGH = on */
#define PIN_FEM_EN     2   /* HIGH = enabled */
#define PIN_FEM_PA     46  /* HIGH only while transmitting */
#endif

#define PIN_OLED_SDA   17
#define PIN_OLED_SCL   18
#define PIN_OLED_RST   21
#define OLED_I2C_ADDR  0x3C

#define PIN_VEXT       36  /* LOW = on */
#define PIN_LED        35
#define PIN_PRG        0

#endif
