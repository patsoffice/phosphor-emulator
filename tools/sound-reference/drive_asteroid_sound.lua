-- drive_asteroid_sound.lua
-- Asteroids attract mode is silent, so we don't need to halt the CPU: just
-- drive the discrete sound inputs directly on a timeline. Each effect is held
-- for a 2 s window so a -wavwrite capture can be segmented by time and analysed
-- for ground-truth pitch (see analyze_wav.py and tools/sound-reference/README.md).
--
-- Run (from the MAME working dir, e.g. ~/mame):
--   mame asteroid -nothrottle -seconds_to_run 18 -video none \
--        -autoboot_script <repo>/tools/sound-reference/drive_asteroid_sound.lua \
--        -wavwrite /tmp/asteroid_ref.wav

local mem
local announced = false

local function all_off(m)
  m:write_u8(0x3600, 0x00)               -- explosion volume 0
  m:write_u8(0x3a00, 0x00)               -- thump disabled
  for line = 0, 5 do
    m:write_u8(0x3c00 + line, 0x00)      -- LS259 audio latch lines off (D7=0)
  end
end

-- {start_seconds, label, setup(mem)}; applied: latest segment whose start <= t.
local timeline = {
  { 1.0,  "thrust",       function(m) m:write_u8(0x3c00 + 3, 0x80) end },
  { 3.0,  "saucer_small", function(m) m:write_u8(0x3c00 + 0, 0x80) end },
  { 5.0,  "saucer_large", function(m) m:write_u8(0x3c00 + 0, 0x80); m:write_u8(0x3c00 + 2, 0x80) end },
  { 7.0,  "life",         function(m) m:write_u8(0x3c00 + 5, 0x80) end },
  { 9.0,  "explosion",    function(m) m:write_u8(0x3600, 0x80 | (0x0f << 2)) end }, -- divider 3, vol 15
  { 11.0, "thump",        function(m) m:write_u8(0x3a00, 0x10 | 0x0f) end },        -- enable + max data
  { 13.0, "ship_fire",    function(m) m:write_u8(0x3c00 + 4, 0x80) end },
  { 15.0, "saucer_fire",  function(m) m:write_u8(0x3c00 + 1, 0x80) end },
  { 17.0, "END",          function(m) end },
}

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end
  if not announced then
    print("[DRIVER] active, driving Asteroids discrete sound")
    announced = true
  end

  -- Elapsed time, not the attotime's integer `seconds` field — that one holds a
  -- whole-second value and quantizes every segment boundary below to 1 s.
  local t = manager.machine.time:as_double()
  all_off(mem)
  local active
  for _, seg in ipairs(timeline) do
    if t >= seg[1] then active = seg end
  end
  if active and active[2] ~= "END" then
    active[3](mem)
  end
end

-- Frame notifier API differs across MAME versions; keep the subscription alive.
_G.__drive_sub = nil
if emu.add_machine_frame_notifier then
  _G.__drive_sub = emu.add_machine_frame_notifier(on_frame)
elseif emu.register_frame_done then
  emu.register_frame_done(on_frame)
else
  print("[DRIVER] ERROR: no frame notifier API available")
end
