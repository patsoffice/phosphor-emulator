-- Run the Road Runner video timing conformance ROM under MAME.
--
-- Design: docs/designs/roadrunner-video-conformance.md
--
-- Usage (see the design doc for the full command line):
--   mame roadrunn -rompath <roms> -autoboot_script tools/mame_roadrunner_conformance.lua
--
-- Environment:
--   PHOSPHOR_CONFORMANCE_BIN   the image to run (default: the committed one)
--   PHOSPHOR_CONFORMANCE_OUT   directory for result.txt and the frame dumps
--   PHOSPHOR_CONFORMANCE_MAX   frame cap (default 300)
--
-- WHAT THIS IS FOR. Every expected value in the conformance suite is derived
-- from *our* board file, which makes the suite a regression guard immediately
-- and a correctness guard only once each figure has been checked against
-- something that is not us. This is that something. The ROM was built for it:
-- it never waits on a cycle count, and every position it reports is divided by
-- an iterations-per-line figure it measures in the same run, so a CPU core with
-- different cycle timings cancels out. That claim has never been tested, and
-- this is the test of it.
--
-- Where the two disagree, the schematic decides, not MAME. That is the standing
-- precedent in this tree (see the note on the M6809's indexed timings in
-- williams-video-conformance.md), and it applies here unchanged.
--
-- HOW THE IMAGE GETS IN. The same trick the Rust harness uses, by a different
-- door. Our harness pokes the program-ROM region through BusDebug; here the
-- image is written into MAME's "maincpu" memory region and the machine is
-- soft-reset so the 68010 re-fetches its vectors out of it. A soft reset does
-- not reload ROMs, so the patch survives. The region is 0x88000 bytes with the
-- motherboard BIOS at 0, which is exactly what the image replaces.
--
-- region:write_u8 takes a LOGICAL offset and handles the host swizzle itself,
-- which matters because the region is 16-bit big-endian and we are almost
-- certainly on a little-endian host. Getting that wrong would byte-swap the
-- image; the ROM's own checksum phase is what would catch it.

-- AN AUTOBOOT SCRIPT IS RE-RUN ON EVERY RESET, INCLUDING THE ONE BELOW. Without
-- this guard the script patches the region, soft-resets, is re-executed, patches
-- again, resets again, and the machine never gets past its first frame. The Lua
-- state itself survives a soft reset, so a global is enough to tell the second
-- execution to stand down and leave the first one's frame notifier running.
if _G.phosphor_conformance_active then
    return
end
_G.phosphor_conformance_active = true

local IMAGE = os.getenv("PHOSPHOR_CONFORMANCE_BIN")
    or "machines/tests/roms/roadrunner_video.bin"
local OUTDIR = os.getenv("PHOSPHOR_CONFORMANCE_OUT") or "."
local MAX_FRAMES = tonumber(os.getenv("PHOSPHOR_CONFORMANCE_MAX") or "300")

-- Result block, mirroring the equates in roadrunner_video.asm.
local RES = 0x400000
local FIELDS = {
    { "R_MAGIC", 0 }, { "R_PHASE", 2 }, { "R_TRAP", 4 }, { "R_TRAPV", 6 },
    { "R_SSP_HI", 8 }, { "R_SSP_LO", 10 }, { "R_CKSUM", 12 }, { "R_VBCOUNT", 14 },
    { "R_T1_BLANK", 16 }, { "R_T1_ACTIVE", 18 }, { "R_T1_BLANK2", 20 },
    { "R_T2_COUNT", 22 }, { "R_T2_HELD", 24 }, { "R_T2_VB", 26 },
    { "R_T3_POLL_A", 28 }, { "R_T3_END_A", 30 }, { "R_T3_POLL_B", 32 },
    { "R_T4_CNT", 34 }, { "R_T4_FIRST", 36 }, { "R_T5_POLL", 38 },
    { "R_TIMEOUT", 40 },
}
local R_MAGIC, R_PHASE = RES + 0, RES + 2
local MAGIC = 0x5A5A

-- Phases whose frame is captured, matching FIRST_SHOT_PHASE and SHOT_COUNT in
-- the Rust harness.
local FIRST_SHOT_PHASE = 11
local LAST_SHOT_PHASE = 16

local frames = 0
local patched = false
local shot_taken = {}
local finished = false

local function log(fmt, ...)
    print(string.format("[CONF] " .. fmt, ...))
end

local function read_image()
    local f, err = io.open(IMAGE, "rb")
    if not f then
        error("cannot open " .. IMAGE .. ": " .. tostring(err))
    end
    local data = f:read("a")
    f:close()
    return data
end

-- Overwrite the start of the maincpu ROM region and restart the CPU on it.
local function patch_and_reset()
    local data = read_image()
    local region = manager.machine.memory.regions[":maincpu"]
    if not region then
        error("no :maincpu memory region")
    end
    if #data > region.size then
        error(string.format("image is %d bytes, region is %d", #data, region.size))
    end
    for i = 1, #data do
        region:write_u8(i - 1, data:byte(i))
    end
    log("patched %d bytes into :maincpu (region %d bytes)", #data, region.size)
    -- The 68010 fetches its stack pointer from 0 and its PC from 4 through the
    -- bus, so it picks up the patched vectors on the way back up.
    manager.machine:soft_reset()
end

-- The visible area is 336x240 with its origin at (0, 0), from the screen's own
-- raw parameters, so these coordinates are the same ones the Rust assertions
-- use and no probe positions have to be restated here. Dumping the whole frame
-- rather than sampling it is what keeps it that way.
local function dump_frame(phase)
    local screen = manager.machine.screens[":screen"]
    if not screen then return end
    local pix, w, h = screen:pixels()
    local rows = { string.format("P6\n%d %d\n255\n", w, h) }
    -- rgb_t packs as 0xAARRGGBB, so on a little-endian host the bytes come out
    -- B, G, R, A and the PPM wants R, G, B.
    local out = {}
    for i = 1, w * h do
        local o = (i - 1) * 4
        out[i] = string.char(pix:byte(o + 3), pix:byte(o + 2), pix:byte(o + 1))
    end
    rows[2] = table.concat(out)
    local path = string.format("%s/mame_phase%d.ppm", OUTDIR, phase)
    local f = io.open(path, "wb")
    if not f then
        log("WARNING could not write %s", path)
        return
    end
    f:write(table.concat(rows))
    f:close()
    log("captured phase %d to %s", phase, path)
end

local function report(reason)
    local mem = manager.machine.devices[":maincpu"].spaces["program"]
    local lines = {}
    for _, fld in ipairs(FIELDS) do
        local v = mem:read_u16(RES + fld[2])
        lines[#lines + 1] = string.format("%s %d", fld[1], v)
        log("%-12s %5d  (0x%04X)", fld[1], v, v)
    end
    lines[#lines + 1] = string.format("FRAMES %d", frames)
    lines[#lines + 1] = string.format("REASON %s", reason)
    log("frames %d, %s", frames, reason)

    local path = OUTDIR .. "/mame_result.txt"
    local f = io.open(path, "w")
    if f then
        f:write(table.concat(lines, "\n") .. "\n")
        f:close()
        log("wrote %s", path)
    else
        log("WARNING could not write %s", path)
    end
end

local function on_frame()
    if finished then return end

    if not patched then
        patch_and_reset()
        patched = true
        return
    end

    frames = frames + 1
    local mem = manager.machine.devices[":maincpu"].spaces["program"]
    local phase = mem:read_u16(R_PHASE)

    if phase >= FIRST_SHOT_PHASE and phase <= LAST_SHOT_PHASE and not shot_taken[phase] then
        shot_taken[phase] = true
        dump_frame(phase)
    end

    if mem:read_u16(R_MAGIC) == MAGIC then
        finished = true
        report("complete")
        manager.machine:exit()
    elseif frames >= MAX_FRAMES then
        finished = true
        report("frame cap reached without the magic word")
        manager.machine:exit()
    end
end

-- The subscription has to stay referenced or it unsubscribes when collected.
_G.phosphor_conformance_sub = emu.add_machine_frame_notifier(on_frame)
log("loaded; image %s, output %s, cap %d frames", IMAGE, OUTDIR, MAX_FRAMES)
