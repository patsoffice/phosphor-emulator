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
--
-- For a LEVEL or a DECAY use drive_asteroid_single.lua instead. The 2 s windows
-- here sit back to back, so no single event is isolated and the analysis has to
-- guess where one voice ends. This driver stays useful for a listen and for
-- spectral shape, which is level-independent.
--
-- Verify it before trusting anything measured against it:
--   tools/sound-reference/verify-reference.sh drive_asteroid_sound.lua asteroid \
--        -seconds_to_run 18

local mem
-- Work RAM holding a JMP to itself. The main CPU is parked there because the
-- game clears the 74LS259 audio latch as housekeeping, about 0.3 ms after this
-- callback sets it, which chops every latch-driven voice into one short burst
-- per frame. See drive_asteroid_single.lua for the full account; the short
-- version is that "attract mode is silent" does not mean the game is not
-- writing, and neither check in verify-reference.sh can see the difference.
local SPIN = 0x0300
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
  -- Park the 6502 and pet the watchdog so a parked CPU cannot reset the board.
  mem:write_u8(SPIN + 0, 0x4c) -- JMP $0300
  mem:write_u8(SPIN + 1, 0x00)
  mem:write_u8(SPIN + 2, 0x03)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN
  mem:write_u8(0x3400, 0)

  local t = manager.machine.time:as_double()

  -- SND_VERIFY drives the two checks in verify-reference.sh: `null` never drives
  -- a segment, so the capture must be silent, and `nudge` shifts the schedule
  -- 30 ms, so the capture must change. Without honouring it the script cannot be
  -- pointed at this driver at all, and its captures stay unverified.
  --
  -- The shift is sub-second on purpose. The integer-seconds bug this guards
  -- against quantised everything to whole seconds and would pass a check that
  -- moved an event by a second.
  local verify = os.getenv("SND_VERIFY")
  if verify == "nudge" then
    t = t - 0.030
  end

  -- Applied unconditionally, including in the null case: holding every line low
  -- is the resting state the measurement assumes, not part of the stimulus.
  all_off(mem)
  if verify == "null" then return end

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
