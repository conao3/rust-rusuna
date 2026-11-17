CREATE TABLE author (id int PRIMARY KEY, name text NOT NULL, nickname text, rating double precision);
CREATE TABLE article (id int PRIMARY KEY, author_id int NOT NULL REFERENCES author(id), title text NOT NULL, published_at timestamptz NOT NULL);
CREATE TABLE comment (id int PRIMARY KEY, article_id int NOT NULL REFERENCES article(id), body text NOT NULL);
CREATE INDEX ON article (author_id);
CREATE INDEX ON article (published_at);
CREATE INDEX ON comment (article_id);
CREATE TABLE revision (article_id int NOT NULL REFERENCES article(id), author_id int NOT NULL REFERENCES author(id), note text NOT NULL, PRIMARY KEY (article_id, author_id, note));
