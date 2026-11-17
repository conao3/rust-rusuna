SELECT coalesce(json_agg(json_build_object('id', a.id, 'title', a.title, 'author', json_build_object('name', au.name), 'comments', c.comments) ORDER BY a.published_at DESC), '[]')
FROM (SELECT * FROM article ORDER BY published_at DESC LIMIT 20) a
JOIN author au ON au.id = a.author_id
LEFT JOIN LATERAL (
  SELECT coalesce(json_agg(json_build_object('body', x.body) ORDER BY x.id), '[]') AS comments
  FROM (SELECT id, body FROM comment WHERE article_id = a.id ORDER BY id LIMIT 5) x
) c ON true
