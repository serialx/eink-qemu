system_ss.add(when: 'CONFIG_CAN_SJA1000', if_true: files('can_sja1000.c'))
system_ss.add(when: 'CONFIG_CAN_PCI', if_true: files('can_kvaser_pci.c'))
system_ss.add(when: 'CONFIG_CAN_PCI', if_true: files('can_pcm3680_pci.c'))
system_ss.add(when: 'CONFIG_CAN_PCI', if_true: files('can_mioe3680_pci.c'))
system_ss.add(when: 'CONFIG_CAN_CTUCANFD', if_true: files('ctucan_core.c'))
system_ss.add(when: 'CONFIG_CAN_CTUCANFD_PCI', if_true: files('ctucan_pci.c'))
system_ss.add(when: 'CONFIG_XLNX_ZYNQMP', if_true: files('xlnx-zynqmp-can.c'))
system_ss.add(when: 'CONFIG_XLNX_VERSAL', if_true: files('xlnx-versal-canfd.c'))
system_ss.add(when: 'CONFIG_CAN_FLEXCAN', if_true: files('flexcan.c'))

# eink-qemu dev-net: Espressif TWAI controllers.
system_ss.add(when: 'CONFIG_XTENSA_ESP32', if_true: files('esp32_twai.c'))
system_ss.add(when: 'CONFIG_RISCV_ESP32C3', if_true: files('esp32_twai.c', 'esp32c3_twai.c'))
system_ss.add(when: 'CONFIG_XTENSA_ESP32S3', if_true: files('esp32_twai.c', 'esp32s3_twai.c'))
