-- drive_dkong_sound.lua
-- Isolate Donkey Kong's discrete walk/jump/stomp effects for capture. Park the
-- main Z80 in a spin loop (so the game makes no sound and stops feeding the 8035
-- sound CPU, which then goes idle and quiets the DAC), then drive the 74LS259
-- sound-trigger latch at 0x7d00-0x7d02 on a timeline:
--   0x7d00 = walk (level), 0x7d01 = jump (one-shot), 0x7d02 = boom/stomp (one-shot).
-- Jump/stomp are edge-triggered, so they're pulsed to re-trigger.
--
-- Run (from the MAME working dir, e.g. ~/mame):
--   mame dkong -nothrottle -seconds_to_run 8 -video none \
--        -autoboot_script <repo>/tools/sound-reference/drive_dkong_sound.lua \
--        -wavwrite /tmp/dkong_ref.wav

local mem
local SPIN = 0x6000 -- main-RAM address holding a JP-to-self

-- {start, end, bit, mode}  mode: "hold" (walk) or "pulse" (one-shot re-trigger)
local timeline = {
  { 1.0, 3.0, 0, "hold" },  -- walk
  { 3.0, 5.0, 1, "pulse" }, -- jump
  { 5.0, 7.0, 2, "pulse" }, -- stomp
}

local function set_bit(b, on)
  mem:write_u8(0x7d00 + b, on and 1 or 0)
end

local function on_frame()
  if not mem then
    local cpu = manager.machine.devices[":maincpu"]
    if not cpu then return end
    mem = cpu.spaces["program"]
  end
  -- Park the main CPU.
  mem:write_u8(SPIN + 0, 0xc3) -- JP nn
  mem:write_u8(SPIN + 1, SPIN & 0xff)
  mem:write_u8(SPIN + 2, (SPIN >> 8) & 0xff)
  manager.machine.devices[":maincpu"].state["PC"].value = SPIN

  local t = manager.machine.time.seconds
  -- All triggers off, then apply the active segment.
  for b = 0, 2 do set_bit(b, false) end
  for _, seg in ipairs(timeline) do
    if t >= seg[1] and t < seg[2] then
      if seg[4] == "hold" then
        set_bit(seg[3], true)
      else
        -- 0.05 s pulse every 0.5 s to re-trigger the one-shot.
        local phase = (t - seg[1]) % 0.5
        set_bit(seg[3], phase < 0.05)
      end
    end
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
