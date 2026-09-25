-- trace_llander_writes.lua
--
-- Log what Lunar Lander's own program writes to its sound hardware while it
-- plays: every change of the 0x3C00 latch, with its time and decoded bits, and
-- a running count of 0x3E00 noise resets. It drives nothing but a coin and a
-- start; the thrust pedal stays released, so the lander falls and crashes on
-- its own, about 30 s after each start.
--
-- A TRACE, NOT A DRIVER. The single-effect drivers park the CPU and write the
-- latch on a scenario's timeline; this leaves the game running and reads what
-- it does, which is the only way to learn how the game uses a circuit whose
-- behavior the drawing leaves open. It was written to answer one question from
-- docs/schematics/llander-audio-output.md: whether a crash holds the explosion
-- at full throttle. It does not. The game sets the explosion and throttle 7
-- together, steps the throttle down one notch every ~0.41 s to 0, holds the
-- explosion bit about 3.5 s longer and clears it.
--
--   mame llander -rompath <roms> -nothrottle -seconds_to_run 120 \
--        -video none -sound none -cfg_directory <fresh> -nvram_directory <fresh> \
--        -autoboot_script tools/sound-reference/trace_llander_writes.lua
--
-- Give each run a fresh cfg and nvram directory, for the reason the README
-- gives: a cfg left by an earlier run changes where the game starts.

local mem = manager.machine.devices[":maincpu"].spaces["program"]
local in1 = manager.machine.ioport.ports[":IN1"]
local last, resets, writes = -1, 0, 0

local function now()
  return manager.machine.time:as_double()
end

local function decode(v)
  return string.format("thrust=%d explode=%d t3k=%d t6k=%d",
    v & 7, (v >> 3) & 1, (v >> 4) & 1, (v >> 5) & 1)
end

-- The taps must stay referenced, or the garbage collector removes them.
_G.__tap_snd = mem:install_write_tap(0x3c00, 0x3c00, "snd", function(offset, data, mask)
  writes = writes + 1
  if data ~= last then
    print(string.format("%9.4f  0x%02x  %s  (writes so far %d, noise resets %d)",
      now(), data, decode(data), writes, resets))
    last = data
  end
end)
_G.__tap_rst = mem:install_write_tap(0x3e00, 0x3e00, "rst", function()
  resets = resets + 1
end)

local events = {
  { 3.0, function() in1.fields["Coin 1"]:set_value(1) end, "coin down" },
  { 3.2, function() in1.fields["Coin 1"]:set_value(0) end, "coin up" },
  { 5.0, function() in1.fields["1 Player Start"]:set_value(1) end, "start down" },
  { 5.2, function() in1.fields["1 Player Start"]:set_value(0) end, "start up" },
}
local next_event = 1

_G.__trace_frame = emu.add_machine_frame_notifier(function()
  while events[next_event] and now() >= events[next_event][1] do
    events[next_event][2]()
    print(string.format("%9.4f  -- %s", now(), events[next_event][3]))
    next_event = next_event + 1
  end
end)
