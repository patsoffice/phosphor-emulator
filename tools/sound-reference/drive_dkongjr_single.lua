-- drive_dkongjr_single.lua
--
-- Capture ONE Donkey Kong Jr. discrete effect, driven the way the game drives
-- it, on the same timeline as the matching `sndcmp` scenario in
-- tools/sound-compare/scenarios/dkongjr/.
--
-- Select the effect with the DKJR_EFFECT environment variable:
--   walk, walk-hi, jump, climb, fall
--
--   DKJR_EFFECT=climb mame dkongjr -rompath <roms> -nothrottle -seconds_to_run 6 \
--        -video none -autoboot_script tools/sound-reference/drive_dkongjr_single.lua \
--        -samplerate 192000 -wavwrite /tmp/dkjr_climb_ref.wav
--
-- `-samplerate 192000` is not optional. MAME's discrete engine simulates the
-- netlist AT the audio sample rate, so a capture's rate is also its simulation
-- rate; at the 48 kHz default this board's few-kHz square edges quantise to
-- 20.8 us and the capture carries broadband hash the circuit does not produce.
-- Resample to a common rate before comparing, or the comparison measures the
-- bandwidth change rather than the circuit.
--
-- Then compare against the Phosphor side:
--
--   sndcmp capture dkongjr/climb --out /tmp/dkjr_climb_ours.wav
--   disasm audiodiff /tmp/dkjr_climb_ours.wav /tmp/dkjr_climb_ref.wav --range-b 1.95:5.0
--
-- THE TRIGGER DISCIPLINE HERE IS THE GAME'S, MEASURED, NOT ASSUMED. Watching
-- both sound latches through 3000 frames of recorded play
-- (tools/script/examples/dkongjr_sound_trace.rhai) gives:
--
--   walk (6H bit 0)    40 assertions, each held exactly 3 frames
--   jump (6H bit 1)     5 assertions, each held exactly 3 frames
--   climb (6H bit 2)    5 assertions, held 0 to 3 frames
--   pitch (6H bit 7)    3 assertions, held 27 to 152 frames
--   fall (5H bit 1)     1 assertion,  held 86 frames
--
-- So three of the four voices get a ~50 ms edge and the fourth gets a 1.4 s
-- level, which is exactly what the drawing says they are: three 74LS123
-- one-shots and one enable with no one-shot anywhere near it. Holding the
-- falling line the way a "sustained voice" scenario would is right for it and
-- would be wrong for any of the others.

local mem
local SPIN = 0x6000 -- main-RAM address holding a JP-to-self

-- The 6H latch at 0x7d00, and the 5H latch at 0x7d80.
local BITS_6H = { walk = 0, jump = 1, climb = 2 }
local FALL_ADDR = 0x7d81
local PITCH_ADDR = 0x7d07

local FRAME_S = 1.0 / 60.0

-- The 2 s pre-roll is not padding. These boards make power-on noise the moment
-- they are switched on, louder than any of the effects, and parking the main Z80
-- does not stop it because the I8035 is already playing by the time the script
-- attaches. Triggering earlier buries the first second of every effect under it.
local PRE_ROLL = 2.0

-- {hold_seconds, uses_pitch_bit}. Three frames is the game's pulse; falling is
-- the one voice held, and 86 frames is what the game holds it for.
local TIMING = {
  walk    = { 3 * FRAME_S, false },
  ["walk-hi"] = { 3 * FRAME_S, true },
  jump    = { 3 * FRAME_S, false },
  climb   = { 3 * FRAME_S, false },
  fall    = { 86 * FRAME_S, false },
}

local effect = os.getenv("DKJR_EFFECT")
if effect == nil or TIMING[effect] == nil then
  print("[DRIVER] ERROR: set DKJR_EFFECT to one of: walk, walk-hi, jump, climb, fall")
  return
end
local hold_s, use_pitch = TIMING[effect][1], TIMING[effect][2]
local assert_s = PRE_ROLL
local release_s = PRE_ROLL + hold_s
print(string.format("[DRIVER] %s: assert %.3f s, release %.3f s, pitch bit %s",
                    effect, assert_s, release_s, tostring(use_pitch)))

local function set_6h(b, on)
  mem:write_u8(0x7d00 + b, on and 1 or 0)
end

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Park the main CPU every frame. Once is not enough: the game may still be
  -- executing elsewhere when the script attaches, and a game that keeps writing
  -- the latch contaminates the capture with its own effects. Asteroids taught
  -- this the expensive way — "the game is quiet here" is a claim needing
  -- evidence, not a reason to skip parking.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN

  -- `manager.machine.time.seconds` is the attotime's INTEGER seconds field, not
  -- elapsed time — it reads 2 for the whole of the third second, which turns
  -- every fractional timeline into a one-second hold and deletes the release
  -- edge entirely. `as_double()` is the accessor that works.
  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the two checks that prove this reference responds to its
  -- own timeline: `null` never asserts, so the capture must be silent, and
  -- `nudge` shifts the schedule 30 ms, so the capture must change.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Hold every trigger low except the one being measured, so a stray latch bit
  -- cannot leak another voice in. Falling matters most here: it is the only
  -- level-driven voice, so a bit left set would sound for the whole capture.
  for _, b in pairs(BITS_6H) do
    set_6h(b, false)
  end
  mem:write_u8(FALL_ADDR, 0)

  -- The walking pitch select is a mode, not a trigger: it is held for the whole
  -- run rather than pulsed, which is how the game uses it.
  mem:write_u8(PITCH_ADDR, use_pitch and 1 or 0)

  if verify == "null" then
    return
  end

  local on = t >= assert_s and t < release_s
  if effect == "fall" then
    mem:write_u8(FALL_ADDR, on and 1 or 0)
  elseif effect == "walk" or effect == "walk-hi" then
    set_6h(BITS_6H.walk, on)
  else
    set_6h(BITS_6H[effect], on)
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
