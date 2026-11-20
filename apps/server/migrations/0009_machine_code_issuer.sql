-- The machine whose core asked for a pairing code, when one did, so that the code goes with that machine.
ALTER TABLE machine_codes ADD COLUMN machine_id TEXT REFERENCES machines (id) ON DELETE CASCADE;

-- For the cascade from a removed machine.
CREATE INDEX machine_codes_machine ON machine_codes (machine_id);
