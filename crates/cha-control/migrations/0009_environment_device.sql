-- The device an environment runs on (docs/devices.md): the id its node's
-- inventory gives it (`nvidia:0`, `vaapi:renderD129`, `cpu`), so placement can
-- count what runs where. NULL: it was launched before devices, on the NVIDIA
-- GPU.
ALTER TABLE environments ADD COLUMN device TEXT;
