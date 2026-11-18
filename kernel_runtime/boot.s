/* Multiboot1 header + entry point for Kernel (.kn) compiled kernels.
 * QEMU's -kernel flag (and GRUB) load this directly in 32-bit protected
 * mode; no custom bootloader needed. */

.set MB_MAGIC, 0x1BADB002
.set MB_FLAGS, 0x0
.set MB_CHECKSUM, -(MB_MAGIC + MB_FLAGS)

.section .multiboot
.align 4
.long MB_MAGIC
.long MB_FLAGS
.long MB_CHECKSUM

.section .bss
.align 16
stack_bottom:
.skip 16384 /* 16 KiB stack */
stack_top:

.section .text
.global _start
_start:
    mov $stack_top, %esp
    call kernel_init
    call kmain
hang:
    cli
    hlt
    jmp hang
