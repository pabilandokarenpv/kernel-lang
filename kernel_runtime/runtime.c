/* Kernel (.kn) v1 freestanding runtime: VGA text-mode console + a few
 * builtins the C codegen backend calls into. This is hand-written support
 * code, not generated — the equivalent of libc's startup glue for a toy OS. */

#include <stdint.h>
#include <stdbool.h>

#define VGA_BUFFER ((volatile uint16_t*)0xB8000)
#define VGA_COLS 80
#define VGA_ROWS 25
#define VGA_ATTR 0x0F /* white on black */

static inline void outb(uint16_t port, uint8_t val) {
    __asm__ volatile ("outb %0, %1" : : "a"(val), "Nd"(port));
}

static void vga_disable_cursor(void) {
    /* BIOS leaves the blinking hardware text cursor on-screen; it visually
     * overlaps whatever character sits at its last position unless we
     * explicitly disable it via the CRTC cursor-disable bit. */
    outb(0x3D4, 0x0A);
    outb(0x3D5, 0x20);
}

static int cursor_row = 0;
static int cursor_col = 0;

static void vga_scroll(void) {
    for (int row = 1; row < VGA_ROWS; row++) {
        for (int col = 0; col < VGA_COLS; col++) {
            VGA_BUFFER[(row - 1) * VGA_COLS + col] = VGA_BUFFER[row * VGA_COLS + col];
        }
    }
    for (int col = 0; col < VGA_COLS; col++) {
        VGA_BUFFER[(VGA_ROWS - 1) * VGA_COLS + col] = (uint16_t)(' ' | (VGA_ATTR << 8));
    }
    cursor_row = VGA_ROWS - 1;
}

void kernel_init(void) {
    vga_disable_cursor();
    for (int row = 0; row < VGA_ROWS; row++) {
        for (int col = 0; col < VGA_COLS; col++) {
            VGA_BUFFER[row * VGA_COLS + col] = (uint16_t)(' ' | (VGA_ATTR << 8));
        }
    }
    cursor_row = 0;
    cursor_col = 0;
}

void kernel_print_char(char c) {
    if (c == '\n') {
        cursor_col = 0;
        cursor_row++;
    } else {
        VGA_BUFFER[cursor_row * VGA_COLS + cursor_col] = (uint16_t)((unsigned char)c | (VGA_ATTR << 8));
        cursor_col++;
        if (cursor_col >= VGA_COLS) {
            cursor_col = 0;
            cursor_row++;
        }
    }
    if (cursor_row >= VGA_ROWS) {
        vga_scroll();
    }
}

void kernel_print_str(const char *s) {
    while (*s) {
        kernel_print_char(*s);
        s++;
    }
}

void kernel_print_long(long n) {
    if (n < 0) {
        kernel_print_char('-');
        n = -n;
    }
    char buf[24];
    int i = 0;
    if (n == 0) {
        buf[i++] = '0';
    }
    while (n > 0) {
        buf[i++] = (char)('0' + (n % 10));
        n /= 10;
    }
    while (i > 0) {
        kernel_print_char(buf[--i]);
    }
}

void kernel_print_bool(bool b) {
    kernel_print_str(b ? "true" : "false");
}

void kernel_print_space(void) {
    kernel_print_char(' ');
}

void kernel_print_newline(void) {
    kernel_print_char('\n');
}

long kernel_abs(long x) {
    return x < 0 ? -x : x;
}
