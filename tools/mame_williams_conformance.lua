-- Run the Williams video timing conformance ROM under MAME.
--
-- Design: docs/designs/williams-video-conformance.md
--
-- Usage (the Rust harness supplies all of these; see the design doc):
--   mame joust -rompath <roms> -autoboot_script tools/mame_williams_conformance.lua
--
-- Environment:
--   PHOSPHOR_CONFORMANCE_BIN    the image to run
--   PHOSPHOR_CONFORMANCE_ADDR   where in the maincpu region it goes, decimal or
--                               0x-prefixed: 0xD000 for joust and robotron,
--                               0xE000 for sinistar
--   PHOSPHOR_CONFORMANCE_OUT    directory for result.txt
--   PHOSPHOR_CONFORMANCE_MAX    frame cap (default 300)
--
-- WHY THIS IS THE SHARPER OF THE TWO CROSS-CHECKS. The Road Runner ROM has no
-- line counter, so every position it reports is in iterations of a poll loop
-- and has to be divided by a rate it measures in the same run; a constant cycle
-- difference between two cores cancels, which is the whole design. Williams has
-- a video counter at $CB00, so its figures are ALREADY ABSOLUTE: 64 transitions
-- a frame, one wrap, a maximum of $FC, count240 at $F0, VA11 edges at
-- $20/$60/$A0/$E0 and $40/$80/$C0, blitter halts of $10 and $20 scanlines.
-- Nothing is calibrated and nothing cancels, so a disagreement is a
-- disagreement.
--
-- HOW THE IMAGE GETS IN. The same trick as the Road Runner script by the same
-- door: write the image into MAME's "maincpu" region and soft-reset so the 6809
-- re-fetches its reset vector out of it. A soft reset does not reload ROMs, so
-- the patch survives. Two things are easier here than there. The region is
-- 8-bit, so region:write_u8 needs no thought about the host swizzle, where the
-- 16-bit big-endian Road Runner region did. And the load address is a plain
-- region offset that matches the CPU address, because the program ROM occupies
-- the top of both.
--
-- AN AUTOBOOT SCRIPT IS RE-RUN ON EVERY RESET, INCLUDING THE ONE BELOW. Without
-- this guard it patches, resets, is re-executed, patches, resets, and never
-- reaches a second frame. The Lua state survives a soft reset, so a global is
-- enough.
if _G.phosphor_williams_active then
    return
end
_G.phosphor_williams_active = true

local IMAGE = os.getenv("PHOSPHOR_CONFORMANCE_BIN")
    or "machines/tests/roms/williams_video.bin"
local ADDR = tonumber(os.getenv("PHOSPHOR_CONFORMANCE_ADDR") or "0xD000")
local OUTDIR = os.getenv("PHOSPHOR_CONFORMANCE_OUT") or "."
local MAX_FRAMES = tonumber(os.getenv("PHOSPHOR_CONFORMANCE_MAX") or "300")

-- Result block at $B000, in an undisplayed video RAM column. Byte-wide, unlike
-- Road Runner's word-wide block in work RAM, because this is a 6809 with an
-- 8-bit bus and there is no read-modify-write to avoid. Mirrors the equates in
-- williams_video.asm; the two arrays are flattened so every line of the output
-- file is one name and one number.
local RES = 0xB000
local FIELDS = {
    { "R_MAGIC", 0 }, { "R_PHASE", 1 },
    { "R_T1TRN", 2 }, { "R_T1WRP", 3 }, { "R_T1MAX", 4 },
    { "R_T1DW0", 5 }, { "R_T1DW4", 6 },
    { "R_T2CNT", 7 }, { "R_T2LIN", 8 },
    { "R_T3RCNT", 9 },
    { "R_T3RLIN0", 10 }, { "R_T3RLIN1", 11 }, { "R_T3RLIN2", 12 }, { "R_T3RLIN3", 13 },
    { "R_T3FCNT", 14 },
    { "R_T3FLIN0", 15 }, { "R_T3FLIN1", 16 }, { "R_T3FLIN2", 17 },
    { "R_T4FST", 18 }, { "R_T4SLW", 19 },
    { "R_T5A", 20 }, { "R_T5B", 21 },
}
local R_MAGIC, R_PHASE = RES + 0, RES + 1
local MAGIC = 0x5A

local frames = 0
local patched = false
local finished = false
local last_phase = -1

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

-- Overwrite the program-ROM window and restart the CPU on it.
local function patch_and_reset()
    local data = read_image()
    local region = manager.machine.memory.regions[":maincpu"]
    if not region then
        error("no :maincpu memory region")
    end
    if ADDR + #data > region.size then
        error(string.format("image is %d bytes at %#x, region is %d",
            #data, ADDR, region.size))
    end
    for i = 1, #data do
        region:write_u8(ADDR + i - 1, data:byte(i))
    end
    log("patched %d bytes at %#x into :maincpu (region %d bytes)",
        #data, ADDR, region.size)
    -- The 6809 fetches its reset vector from $FFFE through the bus, and the
    -- image covers the top of the region, so it picks up the patched vector.
    manager.machine:soft_reset()
end

local function report(reason)
    local mem = manager.machine.devices[":maincpu"].spaces["program"]
    local lines = {}
    for _, fld in ipairs(FIELDS) do
        local v = mem:read_u8(RES + fld[2])
        lines[#lines + 1] = string.format("%s %d", fld[1], v)
        log("%-12s %5d  (0x%02X)", fld[1], v, v)
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
    local phase = mem:read_u8(R_PHASE)
    if phase ~= last_phase then
        log("frame %d: phase %d", frames, phase)
        last_phase = phase
    end

    if mem:read_u8(R_MAGIC) == MAGIC then
        finished = true
        report("complete")
        manager.machine:exit()
    elseif frames >= MAX_FRAMES then
        finished = true
        report("frame cap reached without the magic byte")
        manager.machine:exit()
    end
end

-- The subscription has to stay referenced or it unsubscribes when collected.
--
-- This script captures no pictures, deliberately: Williams renders per scanline
-- and the picture comparison is a separate question with its own capture
-- hazards (phosphor-emulator-fpgx). If one is ever added here, take it with
-- screen:snapshot() and not screen:pixels(); the latter returns the previous
-- frame's pixel indices carrying the current frame's palette, which cost a day
-- on the Road Runner side.
_G.phosphor_williams_sub = emu.add_machine_frame_notifier(on_frame)
log("loaded; image %s at %#x, output %s, cap %d frames", IMAGE, ADDR, OUTDIR, MAX_FRAMES)
