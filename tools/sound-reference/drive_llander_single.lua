-- drive_llander_single.lua
--
-- Capture ONE Lunar Lander discrete voice on the same timeline as the matching
-- `sndcmp` scenario in tools/sound-compare/scenarios/llander/.
--
-- Why this exists alongside drive_llander_sound.lua: that driver walks five
-- segments through 2 s windows back to back, which is fine for a listen and for
-- spectral shape but not for a level. Nothing there isolates one event, and two
-- adjacent windows share a boundary the analysis has to guess at. Asteroids,
-- Galaxian and Donkey Kong each needed the same split.
--
-- Select the voice with LL_EFFECT: thrust, thrust-low, explosion, tone-3k,
-- tone-6k.
--
--   LL_EFFECT=thrust mame llander -rompath <roms> -nothrottle -seconds_to_run 4 \
--        -video none -samplerate 192000 \
--        -autoboot_script tools/sound-reference/drive_llander_single.lua \
--        -wavwrite /tmp/ll_thrust_ref192.wav
--   ffmpeg -i /tmp/ll_thrust_ref192.wav -af aresample=44100:resampler=soxr \
--        /tmp/ll_thrust_ref.wav
--
-- CAPTURE AT 192 kHz AND RESAMPLE. This board is on MAME's DISCRETE engine, not
-- the netlist solver, so the audio sample rate IS the simulation rate: at the
-- 48 kHz default the 3 kHz and 6 kHz squares have their edges quantised to
-- 20.8 us and the capture carries broadband hash the circuit does not make. Two
-- Galaxian voices were written up as having residuals that were entirely this,
-- and one nearly bought a rebuild of a noise source that was already right.
-- Raising the rate also raises the capture BANDWIDTH, so resample both sides to
-- a common rate before comparing anything.
--
-- Then compare against the Phosphor side. `sndcmp capture` writes only the
-- scenario's analysis window, so trim the REFERENCE to the same span rather than
-- re-ranging the capture:
--
--   sndcmp capture llander/thrust --out /tmp/ll_thrust_ours.wav
--   disasm audiodiff /tmp/ll_thrust_ours.wav /tmp/ll_thrust_ref.wav --range-b 0.95:3.0
--
-- THE MAIN CPU IS PARKED, and on this board parking the program counter is not
-- enough on its own. Lunar Lander takes a periodic NMI at MASTER_CLOCK/4096/12,
-- about 246 Hz, so a CPU parked once per frame still vectors into the game's
-- interrupt handler four times between our writes. The generator is gated on the
-- self-test switch -- it fires only while IN0 bit 1 reads high -- so holding
-- Service Mode turns it off at the source. That is what makes this driver's
-- writes the only writes.
--
-- The failure being prevented is Asteroids': there the game cleared the audio
-- latch about 0.3 ms after the driver set it, and every latch-driven voice came
-- out as one short burst per frame rather than a held note. Neither check in
-- verify-reference.sh can see that. A chopped capture is still silent when
-- nothing is triggered and still changes when the schedule moves, so null and
-- sensitivity both pass on a contaminated reference. What gives it away is that
-- the capture is modulated at the machine's frame rate, which is a thing no
-- voice on this board does on its own.

local mem
-- Work RAM holding a JMP to itself. Lunar Lander has 256 bytes of RAM mirrored
-- across 0x0000-0x1FFF, where Asteroids has a full kilobyte, so the 0x0300 the
-- Asteroids driver spins at is a MIRROR of page zero here. Spin at the real
-- address instead, so the bytes written and the bytes executed are visibly the
-- same three.
local SPIN = 0x0000
local WATCHDOG = 0x3400

-- 0x3C00 is one byte: bits 0-2 thrust volume, bit 3 explosion, bit 4 3 kHz tone,
-- bit 5 6 kHz tone.
local function write_sound(reg)
  mem:write_u8(0x3c00, reg)
end

local effect = os.getenv("LL_EFFECT")
local EFFECTS = {
  ["thrust"] = true,
  ["thrust-low"] = true,
  ["explosion"] = true,
  ["tone-3k"] = true,
  ["tone-6k"] = true,
}
if not EFFECTS[effect] then
  print("[DRIVER] ERROR: set LL_EFFECT to one of: thrust, thrust-low, " ..
        "explosion, tone-3k, tone-6k")
  return
end

-- Matches the scenario files: one assert at 1.0 s, held to the end of the run.
-- Held rather than released, because every voice on this board is hard gated --
-- the thrust's "gate" is the volume DAC itself -- so a release gives digital
-- silence and a window carrying a second of that reports the silence.
local TRIGGER_S = 1.0

-- THE EXPLOSION'S VOLUME IS THE THRUST FIELD. Its analog switch takes its signal
-- from the node the three throttle switches drive, so the throttle bits set how
-- loud the explosion is: enabling it with the throttle at zero is digital
-- silence, not a quiet explosion. The board therefore cannot make an explosion
-- in isolation, and neither can this driver. The throttle opens half a second
-- early so the window carries the step from thrust alone to thrust plus
-- explosion, which is the only part of the capture that belongs to the
-- explosion leg. Matches scenarios/llander/explosion.toml.
local EXPLOSION_THROTTLE_S = 0.5

print(string.format("[DRIVER] %s: assert at %.2f s, held to the end", effect, TRIGGER_S))

-- Turn the periodic NMI off at its source by holding the self-test switch. The
-- generator reads IN0 bit 1 and pulses NMI only while it is high; PORT_SERVICE
-- wires that bit active low, so the "on" setting is the one that reads 0.
--
-- SET IT FROM THE FRAME CALLBACK, NOT AT SCRIPT LOAD. An autoboot script runs
-- before the ports are live, so an assignment there is accepted, is written to
-- the cfg on exit, and has no effect on the run that made it. That failure is
-- silent in both directions: nothing errors, and the NEXT run of the same game
-- picks the setting up out of the cfg and behaves correctly. A capture taken
-- before the cfg existed and one taken after it therefore differ by 45 dB with
-- nothing in the command line to say why. Assigning every frame is idempotent
-- and costs nothing.
--
-- `user_value` is the accessor that works. `field:set_value(0)` exists, is
-- accepted, and leaves both `user_value` and the port read unchanged.
local function hold_service()
  pcall(function()
    manager.machine.ioport.ports[":IN0"].fields["Service Mode"].user_value = 0
  end)
end

-- EXCLUSIVITY, checked rather than assumed. Parking the program counter is only
-- a claim until something confirms the CPU stayed parked, and on this board it
-- does not stay parked on its own: the periodic NMI vectors into the game's
-- handler whatever the PC was set to.
--
-- Neither check in verify-reference.sh can see the result. The null run has
-- every voice gated off, so contamination that arrives through a SHARED SOURCE
-- -- here the game strobing the noise shift register's reset at 0x3E00, which
-- puts a strong periodic component through a band-pass with a Q of 7.6 -- is
-- invisible in it, because the gate the contamination would come through is
-- shut. The sensitivity check still passes, because a contaminated capture
-- still changes when the schedule moves. This is the Asteroids lesson one level
-- further in: a shared source is contaminated the same way a shared latch is,
-- and the null check is structurally blind to both.
--
-- Reading the program counter back is a DIRECT check for it and costs two
-- lines. If the CPU is not where it was parked, something else ran.
-- Escapes before this are expected and harmless: the machine boots with the
-- service switch off, `user_value` only takes effect on the frame after it is
-- set, and the stimulus does not start until 1.0 s. What matters is whether the
-- CPU is still loose once the analysis window opens.
local SETTLE_S = 0.5
local escapes, late_escapes = 0, 0
local parked = false

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  hold_service()

  -- Did the CPU stay where it was put? Checked before re-parking, because
  -- re-parking is what would hide the answer.
  local cpu = manager.machine.devices[":maincpu"]
  if parked and cpu.state["PC"].value ~= SPIN then
    escapes = escapes + 1
    if manager.machine.time:as_double() >= SETTLE_S then
      late_escapes = late_escapes + 1
      if late_escapes == 1 then
        print(string.format(
          "[DRIVER] ERROR: the CPU escaped the park at %.3f s (PC 0x%04x, " ..
          "expected 0x%04x). The game is running and driving the sound " ..
          "hardware alongside this driver, and neither of " ..
          "verify-reference.sh's checks can see that. DO NOT USE THIS CAPTURE.",
          manager.machine.time:as_double(), cpu.state["PC"].value, SPIN))
      end
    end
  end

  -- Park the 6502 so the game cannot write the sound register between ours, and
  -- pet the watchdog so a parked CPU does not reset the board.
  mem:write_u8(SPIN + 0, 0x4c) -- JMP $0000
  mem:write_u8(SPIN + 1, 0x00)
  mem:write_u8(SPIN + 2, 0x00)
  cpu.state["PC"].value = SPIN
  parked = true
  mem:write_u8(WATCHDOG, 0)

  -- Elapsed time, not the attotime's integer `seconds` field. That one holds a
  -- whole-second value, so every fractional boundary here would quantise to a
  -- full second and the capture would silently describe a different experiment.
  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the checks in verify-reference.sh: `null` never asserts,
  -- so the capture must be silent, and `nudge` shifts the schedule 30 ms, so the
  -- capture must change. Sub-second on purpose: the bug this guards against
  -- quantised to whole seconds and would sail through a one-second shift.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  local reg = 0x00
  if verify == "null" then
    write_sound(reg)
    return
  end

  if effect == "explosion" then
    if t >= EXPLOSION_THROTTLE_S then reg = 0x07 end
    if t >= TRIGGER_S then reg = 0x0f end
  elseif t >= TRIGGER_S then
    if effect == "thrust" then
      reg = 0x07
    elseif effect == "thrust-low" then
      -- One volume bit, the 15k leg. On the board that is also the darkest
      -- setting of the noise low-pass, because the same resistor sets both.
      reg = 0x01
    elseif effect == "tone-3k" then
      reg = 0x10
    elseif effect == "tone-6k" then
      reg = 0x20
    end
  end
  write_sound(reg)
end

-- Say what the exclusivity check found, pass or fail. A check that only speaks
-- when it fails cannot be told apart from one that never ran.
if emu.add_machine_stop_notifier then
  emu.add_machine_stop_notifier(function()
    if late_escapes > 0 then
      print(string.format("[DRIVER] %d late CPU escapes: THIS CAPTURE IS CONTAMINATED",
                          late_escapes))
    else
      print(string.format(
        "[DRIVER] exclusivity ok: CPU held at the park for the whole window " ..
        "(%d escapes, all before %.1f s)", escapes, SETTLE_S))
    end
  end)
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
