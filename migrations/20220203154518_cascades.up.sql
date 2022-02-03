ALTER TABLE
    auth_state DROP CONSTRAINT auth_state_owner_id_fkey;

ALTER TABLE
    auth_state
ADD
    CONSTRAINT auth_state_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES user_account (id) ON DELETE CASCADE;

ALTER TABLE
    user_session DROP CONSTRAINT user_session_user_id_fkey;

ALTER TABLE
    user_session
ADD
    CONSTRAINT user_session_user_id_fkey FOREIGN KEY (user_id) REFERENCES user_account (id) ON DELETE CASCADE;

ALTER TABLE
    user_setting DROP CONSTRAINT user_setting_owner_id_fkey;

ALTER TABLE
    user_setting
ADD
    CONSTRAINT user_setting_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES user_account (id) ON DELETE CASCADE;
