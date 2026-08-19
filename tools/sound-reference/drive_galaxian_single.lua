-- drive_galaxian_single.lua
--
-- Capture ONE Galaxian discrete voice, triggered ONCE, on the same timeline as
-- the matching `sndcmp` scenario in tools/sound-compare/scenarios/galaxian/.
--
-- Why this exists alongside drive_galaxian_sound.lua: that driver walks four
-- voices through 2 s windows and re-pulses fire and hit every 0.6 s. Overlapping
-- decays hide the envelope and the integrated energy of a single event, so LEVEL
-- and DECAY cannot be measured against it — the same reason Donkey Kong needed
-- drive_dkong_single.lua. The multi-voice driver stays useful for a quick listen
-- and for spectral shape, which is level-independent.
--
-- Select the voice with GAL_EFFECT: melody, fire, hit.
--
--   GAL_EFFECT=fire mame galaxian -rompath <roms> -nothrottle -seconds_to_run 4 \
--        -video none -autoboot_script tools/sound-reference/drive_galaxian_single.lua \
--        -wavwrite /tmp/gal_fire_ref.wav
--
-- Then compare against the Phosphor side. `sndcmp capture` writes only the
-- scenario's analysis window, so trim the reference to the same span:
--
--   sndcmp capture galaxian/fire --out /tmp/gal_fire_ours.wav
--   disasm audiodiff /tmp/gal_fire_ours.wav /tmp/gal_fire_ref.wav --range-b 0.95:3.0
--
-- THE MELODY OSCILLATOR FREE-RUNS. Parking the pitch register high puts its note
-- clock ultrasonic, and both volume lines off keep it out of the mixer; without
-- that, it sits under every other voice's capture at about -29 dBFS. The
-- scenarios express the same thing as `setup = true` actions, and
-- `sndcmp verify` fails without them.

local mem
local SPIN = 0x4000 -- work-RAM address holding a JP-to-self

local function set_pitch(v) mem:write_u8(0x7800, v & 0xff) end
local function set_latch(line, on) mem:write_u8(0x6800 + line, on and 1 or 0) end

-- 74LS259 lines, as wired on the board.
local FS1, FS2, FS3, HIT, FIRE, VOL1, VOL2 = 0, 1, 2, 3, 5, 6, 7

local effect = os.getenv("GAL_EFFECT")
if effect ~= "melody" and effect ~= "fire" and effect ~= "hit" then
  print("[DRIVER] ERROR: set GAL_EFFECT to one of: melody, fire, hit")
  return
end

-- Matches the scenario files: one trigger at 1.0 s, released at 1.05 s, and a
-- run long enough for the decay to finish inside the analysis window.
local TRIGGER_S = 1.0
local RELEASE_S = 1.05

print(string.format("[DRIVER] %s: trigger %.2f s, release %.2f s", effect, TRIGGER_S, RELEASE_S))

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Park the main CPU so the running game stops writing its own sound registers.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN
  -- Pet the watchdog (read of 0x7800) so the parked CPU doesn't trigger a reset.
  mem:read_u8(0x7800)

  -- Elapsed time, not the attotime's integer `seconds` field — that one holds a
  -- whole-second value and would quantize the 50 ms pulse below to a full second.
  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the checks in verify-reference.sh: `null` never triggers,
  -- so the capture must be silent, and `nudge` shifts the schedule 30 ms, so the
  -- capture must change. Sub-second on purpose.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Setup, every frame: park the free-running melody and hold every trigger low.
  -- This is not part of the stimulus, which is why the null check keeps it.
  set_pitch(0xff)
  set_latch(VOL1, false)
  set_latch(VOL2, false)
  for _, line in ipairs({ FS1, FS2, FS3, HIT, FIRE }) do set_latch(line, false) end

  if verify == "null" then return end

  local on = t >= TRIGGER_S and t < RELEASE_S
  if effect == "melody" then
    -- Sustained: held from the trigger to the end of the run, at the pitch the
    -- multi-voice driver uses, so the two agree on what "the melody" is.
    if t >= TRIGGER_S then
      set_pitch(0xB0)
      set_latch(VOL1, true)
      set_latch(VOL2, true)
    end
  elseif effect == "fire" then
    set_latch(FIRE, on)
  elseif effect == "hit" then
    set_latch(HIT, on)
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
