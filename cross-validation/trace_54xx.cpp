// Execution trace of the real Namco 54XX firmware on MAME 0.148's MB88xx,
// wired the way the 54XX device wires it, so our core's trace can be diffed
// against it and the first divergence named.
//
// This is not a vector validator. `validate_mb88xx` already proves the two
// cores agree instruction by instruction, 256000 of 256000. What it cannot
// reach is interrupt entry, the input ports and timing, which is where the
// remaining 54XX defect has to be, so this runs the actual firmware with an
// actual command stream and prints one line per machine cycle.
//
// Usage: trace_54xx <54xx.bin> <cycles> [cmd ...]
//   Each `cmd` is a hex byte delivered in order, one every COMMAND_SPACING
//   cycles, with the chip select held for CS_HOLD cycles around it.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "mb88xx/emu.h"

// The io-space indices MAME's mb88xx.h gives these ports. Repeated here
// because the port handlers below are defined before that header is pulled in
// with mb88xx.c; the enum's order is K, O, P, R0, R1, R2, R3, SI.
enum {
    PORT_K = 0,
    PORT_O = 1,
    PORT_P = 2,
    PORT_R0 = 3,
    PORT_R1 = 4,
};

// Flat memory arrays used by the emu.h shim's address_space stubs
uint8_t mb88_program[2048];
uint8_t mb88_data[128];
uint8_t mb88_io[8];

// What the 54XX presents on its ports. The command's nibbles arrive on two
// different ports because K is only four bits wide.
static uint8_t g_latched_cmd = 0;
// The three channel codes as the ladders see them, latched from port writes.
static uint8_t g_channels[3] = {0, 0, 0};
// Set when a port write happened this cycle, for the trace line.
static char g_port_note[64];

static UINT8 mb88_read(int space_id, offs_t addr) {
    if (space_id == AS_IO) {
        switch (addr & 7) {
            case PORT_K:  return g_latched_cmd >> 4;
            case PORT_R0: return g_latched_cmd & 0x0F;
            default:          return mb88_io[addr & 7];
        }
    }
    if (space_id == AS_DATA) return mb88_data[addr & 0x7F];
    return mb88_program[addr & 0x7FF];
}

static void mb88_write(int space_id, offs_t addr, UINT8 val) {
    if (space_id == AS_IO) {
        int port = addr & 7;
        mb88_io[port] = val;
        if (port == PORT_O) {
            // Bit 4 selects which of the two multiplexed channels the level is
            // for; this is namco54.c's O_w.
            int which = (val & 0x10) ? 1 : 0;
            g_channels[which] = val & 0x0F;
            snprintf(g_port_note, sizeof g_port_note, " O=%02X ch%d<-%X", val,
                     which + 1, val & 0x0F);
        } else if (port == PORT_R1) {
            g_channels[2] = val & 0x0F;
            snprintf(g_port_note, sizeof g_port_note, " R1=%X ch3<-%X",
                     val & 0x0F, val & 0x0F);
        }
        return;
    }
    if (space_id == AS_DATA) {
        mb88_data[addr & 0x7F] = val;
        return;
    }
    mb88_program[addr & 0x7FF] = val;
}

shim_read_fn  shim_mem_read  = mb88_read;
shim_write_fn shim_mem_write = mb88_write;

const attotime attotime::never = attotime{};
const attotime attotime::zero  = attotime{};

#include "mame0148/src/emu/cpu/mb88xx/mb88xx.c"

static legacy_cpu_device g_device;
legacy_cpu_device *shim_active_device = &g_device;
static mb88_state g_state;

static int irq_callback_stub(device_t *, int) { return 0; }

/// Cycles between one command byte and the next, and how long the 06XX holds
/// its chip select, both in MCU machine cycles. Settable because the real
/// board delivers bursts rather than an even stream, and the firmware's
/// response turns out to depend on it.
static int command_spacing() {
    const char *v = getenv("CMD_SPACING");
    return v ? atoi(v) : 400;
}
static int cs_hold() {
    const char *v = getenv("CS_HOLD");
    return v ? atoi(v) : 40;
}

int main(int argc, char *argv[]) {
    if (argc < 3) {
        fprintf(stderr, "Usage: trace_54xx <54xx.bin> <cycles> [hex cmd ...]\n");
        return 1;
    }

    std::ifstream rom(argv[1], std::ios::binary);
    if (!rom.is_open()) {
        fprintf(stderr, "Error: cannot open %s\n", argv[1]);
        return 1;
    }
    std::vector<char> image((std::istreambuf_iterator<char>(rom)),
                            std::istreambuf_iterator<char>());
    memset(mb88_program, 0, sizeof mb88_program);
    memcpy(mb88_program, image.data(), image.size() < 1024 ? image.size() : 1024);

    long cycles = strtol(argv[2], nullptr, 0);
    std::vector<uint8_t> commands;
    for (int i = 3; i < argc; i++) {
        commands.push_back((uint8_t)strtol(argv[i], nullptr, 16));
    }

    memset(&g_state, 0, sizeof(g_state));
    g_device.set_token(&g_state);
    cpu_init_mb88(&g_device, irq_callback_stub);
    cpu_reset_mb88(&g_device);

    size_t next_cmd = 0;
    long next_cmd_at = 200; // let it settle out of reset first
    long cs_until = -1;
    long consumed = 0; // machine cycles, which is what the schedule counts

    for (long iter = 0; consumed < cycles; iter++) {
        long c = consumed;
        if (next_cmd < commands.size() && c >= next_cmd_at) {
            g_latched_cmd = commands[next_cmd++];
            set_irq_line(&g_state, ASSERT_LINE);
            cs_until = c + cs_hold();
            next_cmd_at = c + command_spacing();
            printf("%ld CMD %02X\n", c, g_latched_cmd);
        }
        if (cs_until >= 0 && c >= cs_until) {
            set_irq_line(&g_state, CLEAR_LINE);
            cs_until = -1;
        }

        g_port_note[0] = '\0';
        // Every field is the state BEFORE the instruction runs. Reading any of
        // them afterwards makes the two harnesses disagree about a register the
        // instruction itself changed, which looks exactly like a divergence.
        UINT16 addr = (UINT16)((g_state.PA << 6) | (g_state.PC & 0x3F));
        UINT8 a = g_state.A, y = g_state.Y, st = g_state.st, pio = g_state.pio;

        g_state.icount = 1;
        cpu_execute_mb88(&g_device);
        // One iteration is one instruction here and one machine cycle in the
        // Rust harness, so the command schedule counts cycles rather than
        // iterations or the two get different stimulus.
        consumed += 1 - g_state.icount;

        printf("%ld %04X A=%X Y=%X ST=%d PIO=%X%s\n", consumed, addr, a, y, st,
               pio, g_port_note);
    }

    fprintf(stderr, "channels: ch1=%X ch2=%X ch3=%X\n", g_channels[0],
            g_channels[1], g_channels[2]);
    return 0;
}
