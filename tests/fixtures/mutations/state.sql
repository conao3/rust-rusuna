SELECT json_build_object('mutation_author', (SELECT json_agg(t ORDER BY id) FROM mutation_author t), 'mutation_item', (SELECT json_agg(t ORDER BY id) FROM mutation_item t));
