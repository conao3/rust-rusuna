TRUNCATE mutation_item, mutation_author RESTART IDENTITY CASCADE;
INSERT INTO mutation_author VALUES (1, 'Ada'), (2, 'Bea');
INSERT INTO mutation_item (id, owner_id, title, score, note) VALUES
    (1, 1, 'first', 1, NULL), (2, 2, 'second', 2, NULL), (3, 1, 'hidden', 20, NULL);
