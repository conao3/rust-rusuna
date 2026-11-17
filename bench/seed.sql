INSERT INTO author SELECT g, 'author ' || g FROM generate_series(1, 10000) g;
INSERT INTO article SELECT g, 1 + (g * 7919) % 10000, 'title ' || g, timestamptz '2020-01-01' + (g || ' minutes')::interval FROM generate_series(1, 200000) g;
INSERT INTO comment SELECT g, 1 + (g::bigint * 104729) % 200000, 'comment body ' || g FROM generate_series(1, 1000000) g;
VACUUM ANALYZE;
