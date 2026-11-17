WITH au AS (SELECT id, name FROM author ORDER BY id LIMIT 2000),
c AS (
  SELECT co.article_id, json_agg(json_build_object('body', co.body)) AS comments
  FROM comment co JOIN article a ON a.id = co.article_id JOIN au ON au.id = a.author_id
  GROUP BY co.article_id
),
ar AS (
  SELECT a.author_id, json_agg(json_build_object('id', a.id, 'title', a.title, 'comments', coalesce(c.comments, '[]'))) AS articles
  FROM article a JOIN au ON au.id = a.author_id LEFT JOIN c ON c.article_id = a.id
  GROUP BY a.author_id
)
SELECT coalesce(json_agg(json_build_object('id', au.id, 'name', au.name, 'articles', coalesce(ar.articles, '[]')) ORDER BY au.id), '[]')
FROM au LEFT JOIN ar ON ar.author_id = au.id
