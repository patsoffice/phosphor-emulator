-- Capture an ESB (Star Wars board) lockstep reference from MAME.
--
-- Produces the two halves an instruction-level comparison needs, from ONE run:
--
--   1. the main-CPU instruction trace, and
--   2. every byte the PRNG at $4703 hands the CPU.
--
-- The PRNG matters because MAME implements starwars_prng_r as machine().rand(),
-- a machine-wide LCG that other parts of the driver also draw from. Its
-- sequence therefore cannot be recomputed on our side, only recorded and
-- replayed -- which is what `disasm trace --entropy-file` is for. It has to
-- come from the same run as the trace, or the two drift apart.
--
-- Usage:
--
--   rm -rf "$OUT/nvram" "$OUT/cfg"        # cold boot: MAME reloads NVRAM it
--   mkdir -p "$OUT/nvram" "$OUT/cfg"      # wrote on a previous run otherwise
--
--   cd "$OUT" && SDL_VIDEODRIVER=dummy \
--     TRACE_OUT=$OUT/mame_raw.txt ENTROPY_OUT=$OUT/entropy.txt RUN_FRAMES=90 \
--     mame esb -rompath <roms> \
--       -cfg_directory "$OUT/cfg" -nvram_directory "$OUT/nvram" \
--       -video none -sound none -nothrottle \
--       -debug -debugger none -autoboot_delay 0 \
--       -autoboot_script tools/mame_esb_lockstep.lua
--
--   disasm trace --machine esb --frames 90 --cpu 0:regs \
--     --entropy-file "$OUT/entropy.txt" -o "$OUT/ours.txt" <roms>
--
--   tools/lockstep_diff.py "$OUT/ours.txt" "$OUT/mame_raw.txt"
--
-- Two things about the flags are load-bearing:
--
--   * `-debugger none` never pumps the debugger's command loop, so a
--     `-debugscript` is read but never executed. The trace command is issued
--     from Lua here instead, which does work.
--   * `-autoboot_delay 0` fires this script at reset, before the first CPU
--     cycle, so the trace starts where ours does. The default delay would drop
--     the first seconds of execution.
--
-- Clearing the NVRAM directory matters just as much: MAME writes NVRAM at
-- exit, so a second run cold-boots from the first run's saved state while
-- phosphor starts from a blank X2212, and the traces split at the first
-- $4500-$45FF read.

local TRACE = os.getenv("TRACE_OUT") or "mame_raw.txt"
local OUT = os.getenv("ENTROPY_OUT") or "entropy.txt"
local FRAMES = tonumber(os.getenv("RUN_FRAMES") or "90")

_G.ls_f = io.open(OUT, "w")
_G.ls_screen = manager.machine.screens:at(1)
_G.ls_main = manager.machine.devices[":maincpu"]
_G.ls_sp = _G.ls_main.spaces["program"]
_G.ls_done = false

_G.ls_tap = _G.ls_sp:install_read_tap(0x4703, 0x4703, "prng",
  function(offset, data, mask)
    _G.ls_f:write(string.format("%02X\n", data & 0xff))
  end)

-- "%d %04X " prefixes each trace line with the cycle count and D, ahead of
-- MAME's own "PC: disassembly". lockstep_diff.py parses exactly that shape.
manager.machine.debugger:command(
  string.format('trace %s,maincpu,noloop,{tracelog "%%d %%04X ",totalcycles,d}', TRACE))

_G.ls_sub = emu.add_machine_frame_notifier(
  function()
    if _G.ls_done then return end
    if _G.ls_screen:frame_number() >= FRAMES then
      _G.ls_done = true
      manager.machine.debugger:command("trace off")
      _G.ls_f:flush()
      _G.ls_f:close()
      manager.machine:exit()
    end
  end)
