CREATE OR REPLACE FUNCTION search_articles(search text, since_id int) RETURNS SETOF article LANGUAGE sql STABLE AS $$
  SELECT * FROM article WHERE title LIKE search AND (since_id IS NULL OR id >= since_id)
$$;
CREATE OR REPLACE FUNCTION "author totals"(min_articles int) RETURNS TABLE(author_id int, articles int, first_title text) LANGUAGE sql STABLE AS $$
  SELECT author_id, count(*)::int, min(title) FROM article GROUP BY author_id HAVING count(*) >= min_articles
$$;
CREATE OR REPLACE FUNCTION latest_titles() RETURNS TABLE(author_id int, title text) LANGUAGE sql STABLE AS $$
  SELECT DISTINCT ON (author_id) author_id, title FROM article ORDER BY author_id, published_at DESC
$$;
