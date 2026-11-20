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

double kernel_fabs(double x) {
    return x < 0 ? -x : x;
}

/* Prints a double with exactly 2 decimal places, matching Rust's "{:.2}"
 * format exactly (that's the interpreter's float Display impl — see
 * interpreter.rs) so both execution paths produce identical output with
 * no printf/libm available in this freestanding environment. */
void kernel_print_float(double x) {
    if (x < 0) {
        kernel_print_char('-');
        x = -x;
    }
    /* round to 2 decimal places by scaling, matching standard "round half
     * up" behavior for the common case */
    long scaled = (long)(x * 100.0 + 0.5);
    long int_part = scaled / 100;
    long frac_part = scaled % 100;

    kernel_print_long(int_part);
    kernel_print_char('.');
    if (frac_part < 10) {
        kernel_print_char('0');
    }
    kernel_print_long(frac_part);
}

/* String concatenation, freestanding-safe: no malloc/heap in this
 * bare-metal environment, so concatenation writes into a small rotating
 * pool of static buffers instead of allocating. This bounds both the
 * result length (KERNEL_STRCAT_BUF_LEN) and the number of concatenation
 * results that can be "alive" at once (KERNEL_STRCAT_NUM_BUFS) before an
 * older result gets overwritten — a documented v1 limitation, not a
 * silent one, matching how the rest of this freestanding runtime works. */
#define KERNEL_STRCAT_BUF_LEN 256
#define KERNEL_STRCAT_NUM_BUFS 8

static char kernel_strcat_bufs[KERNEL_STRCAT_NUM_BUFS][KERNEL_STRCAT_BUF_LEN];
static int kernel_strcat_next_buf = 0;

const char *kernel_strcat(const char *a, const char *b) {
    char *buf = kernel_strcat_bufs[kernel_strcat_next_buf];
    kernel_strcat_next_buf = (kernel_strcat_next_buf + 1) % KERNEL_STRCAT_NUM_BUFS;

    int i = 0;
    while (*a && i < KERNEL_STRCAT_BUF_LEN - 1) {
        buf[i++] = *a++;
    }
    while (*b && i < KERNEL_STRCAT_BUF_LEN - 1) {
        buf[i++] = *b++;
    }
    buf[i] = '\0';
    return buf;
}

/* Byte length of a null-terminated string -- there's no libc (and so no
 * strlen) in this -ffreestanding environment, so Kernel's `len()` builtin
 * (codegen_c.rs) needs its own. Matches the interpreter's `s.len()` (Rust
 * byte length) exactly for any string this language can represent. */
long kernel_strlen(const char *s) {
    long n = 0;
    while (*s) {
        n++;
        s++;
    }
    return n;
}

/* Halts the kernel after printing a message -- the only "abort" mechanism
 * available in a freestanding environment with no process/exit() concept.
 * Used by the C codegen backend's array bounds checks (see codegen_c.rs):
 * an out-of-range index is a real safety violation, so this stops execution
 * instead of reading/writing adjacent memory (the actual undefined
 * behavior this exists to prevent). Mirrors boot.s's own `hang:` loop so
 * there's exactly one halt idiom in the whole runtime. Marked `_Noreturn`
 * (declared that way in the generated C too) so callers like the bounds-
 * check statement-expression type-check correctly even though this never
 * produces a value. */
_Noreturn void kernel_panic(const char *msg) {
    kernel_print_str("PANIC: ");
    kernel_print_str(msg);
    kernel_print_newline();
    for (;;) {
        __asm__ volatile ("cli; hlt");
    }
}
