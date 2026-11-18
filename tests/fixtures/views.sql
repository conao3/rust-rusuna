CREATE TABLE view_author (id integer PRIMARY KEY, name text NOT NULL);
CREATE TABLE view_article (
    id integer PRIMARY KEY,
    author_id integer NOT NULL REFERENCES view_author(id),
    title text NOT NULL,
    score integer NOT NULL,
    published boolean NOT NULL
);
CREATE VIEW view_published AS
    SELECT id, author_id, title, score FROM view_article WHERE published;
CREATE MATERIALIZED VIEW view_author_stats AS
    SELECT author_id, count(*)::integer AS articles, sum(score)::integer AS total
    FROM view_article GROUP BY author_id;
INSERT INTO view_author VALUES (1, 'Ada'), (2, 'Bea'), (3, 'Cy');
INSERT INTO view_article VALUES
    (1, 1, 'first', 3, true),
    (2, 1, 'draft', 5, false),
    (3, 2, 'second', 7, true),
    (4, 2, 'third', 2, true);
REFRESH MATERIALIZED VIEW view_author_stats;
