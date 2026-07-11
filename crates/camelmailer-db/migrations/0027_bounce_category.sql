-- Bounce classification: terminally failed deliveries and processed
-- bounce messages carry a category derived from the SMTP status / DSN
-- diagnostic fields (5xx permanent -> hard, 4xx -> soft, otherwise
-- undetermined). NULL = not (yet) classified; the API and the stats
-- breakdown treat unclassified bounces as undetermined.

ALTER TABLE messages
    ADD COLUMN bounce_category TEXT
        CHECK (bounce_category IN ('hard', 'soft', 'undetermined'));
