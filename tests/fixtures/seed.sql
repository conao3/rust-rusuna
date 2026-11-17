INSERT INTO author VALUES (1, 'author 1'), (2, 'author 2'), (3, 'author 3'), (4, 'author 4'), (5, 'author 5'), (6, 'author 6');
INSERT INTO article VALUES
  (1, 1, 'title 1', '2020-01-01 00:01+00'),
  (2, 2, 'title 2', '2020-01-01 00:12+00'),
  (3, 3, 'title 3', '2020-01-01 00:03+00'),
  (4, 3, 'title 4', '2020-01-01 00:10+00'),
  (5, 3, 'title 5', '2020-01-01 00:05+00'),
  (6, 4, 'title 6', '2020-01-01 00:06+00'),
  (7, 1, 'title 7', '2020-01-01 00:07+00'),
  (8, 2, 'title 8', '2020-01-01 00:08+00'),
  (9, 3, 'title 9', '2020-01-01 00:09+00'),
  (10, 5, 'title 10', '2020-01-01 00:04+00'),
  (11, 5, 'title 11', '2020-01-01 00:11+00'),
  (12, 1, 'title 12', '2020-01-01 00:02+00');
INSERT INTO comment VALUES
  (1, 1, 'comment body 1'), (2, 1, 'comment body 2'), (3, 1, 'comment body 3'), (4, 1, 'comment body 4'),
  (5, 1, 'comment body 5'), (6, 1, 'comment body 6'), (7, 1, 'comment body 7'), (8, 2, 'comment body 8'),
  (9, 3, 'comment body 9'), (10, 9, 'comment body 10'), (11, 9, 'comment body 11'), (12, 9, 'comment body 12'),
  (13, 4, 'comment body 13'), (14, 5, 'comment body 14'), (15, 10, 'comment body 15'), (16, 10, 'comment body 16'),
  (17, 12, 'comment body 17'), (18, 12, 'it''s "quoted"'), (19, 3, 'it''s "quoted"'), (20, 7, 'comment body 20');
INSERT INTO revision VALUES
  (1, 1, 'first'), (1, 1, 'second'), (1, 2, 'by another author'), (3, 3, 'typo'), (4, 3, 'title'),
  (4, 3, 'body'), (9, 3, 'draft'), (10, 5, 'draft'), (12, 2, 'by another author');
ANALYZE;
