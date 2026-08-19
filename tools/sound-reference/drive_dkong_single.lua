-- drive_dkong_single.lua
--
-- Capture ONE Donkey Kong discrete effect, triggered ONCE, on the same timeline
-- as the matching `sndcmp` scenario in tools/sound-compare/scenarios/dkong/.
--
-- Why this exists alongside drive_dkong_sound.lua: that driver re-pulses jump
-- and stomp every 0.5 s inside a 2 s window. Overlapping decays hide the
-- envelope and the integrated energy of a single event, so LEVEL and DECAY
-- cannot be measured against it — an isolated single trigger reads ~3 dB
-- quieter than that window simply because the window holds more than one hit.
-- The multi-effect driver stays useful for a quick listen and for comparing
-- spectral shape, which is level-independent.
--
-- Select the effect with the DK_EFFECT environment variable: walk, jump, stomp.
--
--   DK_EFFECT=stomp mame dkong -rompath <roms> -nothrottle -seconds_to_run 6 \
--        -video none -autoboot_script tools/sound-reference/drive_dkong_single.lua \
--        -wavwrite /tmp/dk_stomp_ref.wav
--
-- Then compare against the Phosphor side. `sndcmp capture` writes only the
-- scenario's analysis window, so trim the reference to the same span:
--
--   sndcmp capture dkong/stomp --out /tmp/dk_stomp_ours.wav
--   disasm audiodiff /tmp/dk_stomp_ours.wav /tmp/dk_stomp_ref.wav --range-b 1.95:5.0
--
-- Timelines are the scenario files', kept identical on purpose:
--   walk   assert 2.0 s, release 4.0 s, run 5.0 s   (sustained + release tail)
--   jump   pulse 2.0-2.05 s, run 5.0 s              (one-shot, full decay)
--   stomp  pulse 2.0-2.05 s, run 5.0 s              (one-shot, full decay)

local mem
local SPIN = 0x6000 -- main-RAM address holding a JP-to-self

-- Latch bit per effect (74LS259 at 0x7d00-0x7d02).
local BITS = { walk = 0, jump = 1, stomp = 2 }

-- {assert_s, release_s} per effect, matching the scenario files.
--
-- The 2 s pre-roll is not padding. Donkey Kong makes power-on noise the moment
-- it is switched on — measured at -11 dBFS, LOUDER than any of the effects —
-- and parking the main Z80 does not stop it, because by the time the script
-- attaches the I8035 is already playing and has to run itself out. It decays
-- below -57 dBFS by 1.5 s. Triggering at 0.5 s, as the multi-effect driver
-- does, buries the first second of every effect under it.
local PRE_ROLL = 2.0
local TIMING = {
  walk  = { PRE_ROLL, PRE_ROLL + 2.0 },
  jump  = { PRE_ROLL, PRE_ROLL + 0.05 },
  stomp = { PRE_ROLL, PRE_ROLL + 0.05 },
}

local effect = os.getenv("DK_EFFECT")
if effect == nil or BITS[effect] == nil then
  print("[DRIVER] ERROR: set DK_EFFECT to one of: walk, jump, stomp")
  print("[DRIVER]   e.g. DK_EFFECT=stomp mame dkong ... -autoboot_script this.lua")
  return
end
local bit = BITS[effect]
local assert_s, release_s = TIMING[effect][1], TIMING[effect][2]
print(string.format("[DRIVER] %s: bit %d, assert %.2f s, release %.2f s",
                    effect, bit, assert_s, release_s))

local function set_bit(b, on)
  mem:write_u8(0x7d00 + b, on and 1 or 0)
end

-- The analysis window opens at 0.45 s, so everything before it must be silent.
-- Parking the main Z80 stops the game feeding the I8035, which then goes idle
-- and quiets the DAC — but that takes a few frames, and the board makes its
-- power-on noise in the meantime. The effect is not triggered until 0.5 s,
-- which leaves the settling time inside the pre-roll rather than inside the
-- measurement.
local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Park the main CPU every frame: writing the spin loop once is not enough,
  -- since the game may still be executing elsewhere when the script attaches.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN

  local t = manager.machine.time.seconds

  -- Hold every trigger low except the one this run is measuring, so a stray
  -- latch bit cannot leak another voice into the capture.
  for _, b in pairs(BITS) do
    if b ~= bit then set_bit(b, false) end
  end

  -- One assert, one release. No re-pulsing: that is the entire point.
  set_bit(bit, t >= assert_s and t < release_s)
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
