SELECT coalesce(json_agg(json_build_object('id', a.id, 'title', a.title, 'author', json_build_object('name', au.name)) ORDER BY a.id), '[]')
FROM (SELECT * FROM article ORDER BY id LIMIT 50000) a
JOIN author au ON au.id = a.author_id
