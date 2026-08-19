-- drive_galaxian_sound.lua
-- Isolate Galaxian's discrete sound voices for capture. Park the main Z80 in a
-- spin loop (so the running game stops writing its own sound registers), pet the
-- watchdog so MAME doesn't reset, then drive the sound registers on a timeline:
--   0x7800      = background-melody pitch latch (8 bit)
--   0x6004-7    = LFO / "wolf-whistle" DAC (4 lines, bit 0 each)
--   0x6800-7    = 74LS259 sound latch: FS1/2/3=lines 0-2, HIT=3, FIRE=5,
--                 VOL1/2=lines 6-7
--
-- Timeline (one voice per 2 s window): tune, wolf-whistle, fire, hit.
--
-- Run (from ~/ws/mame-runtime with the galaxian romset):
--   mame galaxian -nothrottle -seconds_to_run 10 -video none \
--        -autoboot_script <repo>/tools/sound-reference/drive_galaxian_sound.lua \
--        -wavwrite /tmp/galaxian_ref.wav

local mem
local SPIN = 0x4000 -- work-RAM address holding a JP-to-self

local function set_pitch(v) mem:write_u8(0x7800, v & 0xff) end
local function set_lfo(v)              -- 4-bit DAC across 0x6004-0x6007
  for b = 0, 3 do mem:write_u8(0x6004 + b, (v >> b) & 1) end
end
local function set_latch(line, on) mem:write_u8(0x6800 + line, on and 1 or 0) end

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Park the main CPU in a JP-to-self loop.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN
  -- Pet the watchdog (read of 0x7800) so the parked CPU doesn't trigger a reset.
  mem:read_u8(0x7800)

  -- Elapsed time, not the attotime's integer `seconds` field — that one holds a
  -- whole-second value and quantizes every segment boundary below to 1 s.
  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the two checks that prove this reference responds to its
  -- own timeline. `null` runs the schedule with nothing ever asserted, so the
  -- capture must be silent; `nudge` shifts every boundary by 30 ms, so the
  -- capture must change. A driver that ignores its timeline passes neither, and
  -- that is precisely the failure that made every capture here a held line
  -- rather than a pulse. The shift is deliberately sub-second: the bug it looks
  -- for quantised to whole seconds and would swallow anything coarser.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Baseline: everything off. Pitch is parked high (0xFF) so the always-on
  -- melody note clock is ultrasonic (silent) and doesn't bleed into the other
  -- voices' windows.
  set_pitch(0xff)
  set_lfo(0)
  for line = 0, 7 do set_latch(line, false) end

  if verify == "null" then
    return -- everything already parked off above
  end

  if t >= 1.0 and t < 3.0 then
    -- Background melody: steady pitch, mixer volume on.
    set_pitch(0xB0)
    set_latch(6, true) -- VOL1
    set_latch(7, true) -- VOL2
  elseif t >= 3.0 and t < 5.0 then
    -- Wolf-whistle: the three FS oscillators swept by the LFO DAC (0->15).
    set_latch(0, true) -- FS1
    set_latch(1, true) -- FS2
    set_latch(2, true) -- FS3
    set_latch(6, true)
    set_latch(7, true)
    local frac = (t - 3.0) / 2.0
    set_lfo(math.floor(frac * 15.999))
  elseif t >= 5.0 and t < 7.0 then
    -- Fire / shoot: pulse the FIRE line (one-shot) to re-trigger.
    set_latch(5, ((t - 5.0) % 0.6) < 0.1)
  elseif t >= 7.0 and t < 9.0 then
    -- Hit / explosion: pulse the HIT line.
    set_latch(3, ((t - 7.0) % 0.6) < 0.1)
  end
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
