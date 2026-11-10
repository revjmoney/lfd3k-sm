/* Heltec V4 (plain V4 OLED, GC1109 front end) pin map. */
#ifndef BOARD_H
#define BOARD_H

#define PIN_LORA_NSS   8
#define PIN_LORA_SCK   9
#define PIN_LORA_MOSI  10
#define PIN_LORA_MISO  11
#define PIN_LORA_RST   12
#define PIN_LORA_BUSY  13
#define PIN_LORA_DIO1  14

/* GC1109 front end */
#define PIN_FEM_POWER  7   /* LDO, HIGH = on */
#define PIN_FEM_EN     2   /* HIGH = enabled */
#define PIN_FEM_PA     46  /* HIGH only while transmitting */

#define PIN_OLED_SDA   17
#define PIN_OLED_SCL   18
#define PIN_OLED_RST   21
#define OLED_I2C_ADDR  0x3C

#define PIN_VEXT       36  /* LOW = on */
#define PIN_LED        35
#define PIN_PRG        0

#endif
