-- drive_llander_sound.lua
-- Drive the Lunar Lander discrete sound register (0x3c00) on a timeline so a
-- -wavwrite capture can be segmented and compared against Phosphor. The sound
-- register is a single byte: bits 0-2 thrust volume, bit 3 explosion, bit 4
-- 3 kHz tone, bit 5 6 kHz tone. (Explosion reuses the thrust value as its
-- volume, so the explosion segment also sets thrust bits.)
--
-- Run (from the MAME working dir, e.g. ~/mame):
--   mame llander -nothrottle -seconds_to_run 12 -video none \
--        -autoboot_script <repo>/tools/sound-reference/drive_llander_sound.lua \
--        -wavwrite /tmp/llander_ref.wav

local mem

-- {start_seconds, label, register_value}; applied: latest whose start <= t.
local timeline = {
  { 1.0,  "thrust_full", 0x07 },
  { 3.0,  "thrust_low",  0x02 },
  { 5.0,  "tone3k",      0x10 },
  { 7.0,  "tone6k",      0x20 },
  { 9.0,  "explosion",   0x0f }, -- thrust 7 + explosion enable
  { 11.0, "END",         0x00 },
}

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end
  local t = manager.machine.time.seconds
  local value = 0x00
  for _, seg in ipairs(timeline) do
    if t >= seg[1] then value = seg[3] end
  end
  mem:write_u8(0x3c00, value)
end

_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
