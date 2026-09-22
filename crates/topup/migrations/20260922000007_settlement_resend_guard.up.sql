ALTER TABLE settlements
    ADD COLUMN resend_forbidden boolean NOT NULL DEFAULT false;
