-- Physical postal address included in the CAN-SPAM compliance footer of
-- broadcast mail (NULL = no address configured yet).
ALTER TABLE servers ADD COLUMN broadcast_physical_address TEXT;
