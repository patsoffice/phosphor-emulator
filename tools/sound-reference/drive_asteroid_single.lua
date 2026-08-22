-- drive_asteroid_single.lua
--
-- Capture ONE Asteroids discrete voice on the same timeline as the matching
-- `sndcmp` scenario in tools/sound-compare/scenarios/asteroids/.
--
-- Why this exists alongside drive_asteroid_sound.lua: that driver walks eight
-- voices through 2 s windows back to back, which is fine for a listen and for
-- spectral shape but not for a level or a decay. Nothing there isolates a single
-- event, and two adjacent windows share a boundary the analysis has to guess at.
-- Galaxian and Donkey Kong each needed the same split.
--
-- Select the voice with AST_EFFECT: thrust, explosion, thump, saucer,
-- saucer-large, ship-fire, saucer-fire, life.
--
--   AST_EFFECT=thrust mame asteroid -rompath <roms> -nothrottle -seconds_to_run 4 \
--        -video none -samplerate 192000 \
--        -autoboot_script tools/sound-reference/drive_asteroid_single.lua \
--        -wavwrite /tmp/ast_thrust_ref192.wav
--   ffmpeg -i /tmp/ast_thrust_ref192.wav -af aresample=44100:resampler=soxr \
--        /tmp/ast_thrust_ref.wav
--
-- CAPTURE AT 192 kHz AND RESAMPLE. The discrete engine simulates the netlist at
-- the audio sample rate, so at the 48 kHz default a capture reports its own edge
-- quantisation as broadband hash the circuit does not make. Two Galaxian voices
-- were written up as having residuals that were entirely this, and one of them
-- nearly bought a rebuild of a noise source that was already right. Raising the
-- rate also raises the capture BANDWIDTH, so resample both sides to a common rate
-- before comparing anything.
--
-- Then compare against the Phosphor side. `sndcmp capture` writes only the
-- scenario's analysis window, so trim the reference to the same span rather than
-- re-ranging the capture:
--
--   sndcmp capture asteroids/thrust --out /tmp/ast_thrust_ours.wav
--   disasm audiodiff /tmp/ast_thrust_ours.wav /tmp/ast_thrust_ref.wav --range-b 0.95:3.0
--
-- ATTRACT MODE IS SILENT on this board, so unlike Galaxian and Donkey Kong the
-- main CPU does not need parking: it is not writing sound registers while this
-- runs. The all-off pass below is still applied every frame, so a game write
-- during a longer run cannot leak into a capture.

local mem

-- Board writes, as decoded at 0x3600 / 0x3A00 / 0x3C00+n.
local function write_explosion(vol, pitch_sel)
  mem:write_u8(0x3600, ((pitch_sel & 0x03) << 6) | ((vol & 0x0f) << 2))
end
local function write_thump(enable, data)
  mem:write_u8(0x3a00, (enable and 0x10 or 0x00) | (data & 0x0f))
end
local function set_latch(line, on)
  mem:write_u8(0x3c00 + line, on and 0x80 or 0x00)
end

-- 74LS259 lines, as wired on the board.
local SAUCER, SAUCER_FIRE, SAUCER_SEL, THRUST, SHIP_FIRE, LIFE = 0, 1, 2, 3, 4, 5

local effect = os.getenv("AST_EFFECT")
local EFFECTS = {
  ["thrust"] = true,
  ["explosion"] = true,
  ["thump"] = true,
  ["saucer"] = true,
  ["saucer-large"] = true,
  ["ship-fire"] = true,
  ["saucer-fire"] = true,
  ["life"] = true,
}
if not EFFECTS[effect] then
  print("[DRIVER] ERROR: set AST_EFFECT to one of: thrust, explosion, thump, " ..
        "saucer, saucer-large, ship-fire, saucer-fire, life")
  return
end

-- Matches the scenario files: one assert at 1.0 s, held to the end of the run.
-- Held rather than released, because every voice on this board is hard gated: a
-- release gives digital silence, and a window carrying a second of that reports
-- the silence rather than the effect.
local TRIGGER_S = 1.0

-- Explosion volume and noise-divider select, matching scenarios/asteroids/
-- explosion.toml. Divider select 2 is the 12 kHz / 3 re-clock.
local EXPL_VOL, EXPL_PITCH = 15, 2
-- Thump DAC code, matching thump.toml: the maximum, which is the lowest pitch the
-- board can ask for and the furthest from the DAC's linear region.
local THUMP_DATA = 15

print(string.format("[DRIVER] %s: assert at %.2f s, held to the end", effect, TRIGGER_S))

local function all_off()
  write_explosion(0, 0)
  write_thump(false, 0)
  for line = 0, 5 do set_latch(line, false) end
end

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end

  -- Elapsed time, not the attotime's integer `seconds` field. That one holds a
  -- whole-second value, so every fractional boundary here would quantise to a
  -- full second and the capture would silently describe a different experiment.
  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the checks in verify-reference.sh: `null` never asserts, so
  -- the capture must be silent, and `nudge` shifts the schedule 30 ms, so the
  -- capture must change. Sub-second on purpose: the bug this guards against
  -- quantised to whole seconds and would sail through a one-second shift.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Every frame, so a game write cannot leak in. Not part of the stimulus, which
  -- is why the null case keeps it.
  all_off()
  if verify == "null" then return end
  if t < TRIGGER_S then return end

  if effect == "thrust" then
    set_latch(THRUST, true)
  elseif effect == "explosion" then
    write_explosion(EXPL_VOL, EXPL_PITCH)
  elseif effect == "thump" then
    write_thump(true, THUMP_DATA)
  elseif effect == "saucer" then
    set_latch(SAUCER_SEL, false)
    set_latch(SAUCER, true)
  elseif effect == "saucer-large" then
    set_latch(SAUCER_SEL, true)
    set_latch(SAUCER, true)
  elseif effect == "ship-fire" then
    -- HELD, not pulsed, matching ship-fire.toml. Whether the board holds this
    -- line is an open question; what is certain is that our model's chirp is
    -- driven off the enable LEVEL and truncates the moment it drops. Both sides
    -- hold it so they describe the same event, and both change together when the
    -- trigger discipline is settled against the netlist.
    set_latch(SHIP_FIRE, true)
  elseif effect == "saucer-fire" then
    set_latch(SAUCER_FIRE, true)
  elseif effect == "life" then
    set_latch(LIFE, true)
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
