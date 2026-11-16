# Sourced by the MySQL image entrypoint on first start; `docker_process_sql`
# runs SQL as root against the temporary init server.
#
# `rustmy` (created by the image) uses caching_sha2_password, the default.
# Add a mysql_native_password account where the plugin still exists (8.4 with
# --mysql-native-password=ON); MySQL 9.x removed it.

native_available="$(docker_process_sql --skip-column-names <<-'EOSQL'
	SELECT COUNT(*) FROM information_schema.PLUGINS
	WHERE PLUGIN_NAME = 'mysql_native_password' AND PLUGIN_STATUS = 'ACTIVE';
EOSQL
)"

# SQLAlchemy's dialect suite (tests/sqla_suite): a database of its own, since
# its reflection tests list every table (benchmarks leave theirs in
# rustmy_test), and the two extra schemas it expects.
docker_process_sql <<-'EOSQL'
	CREATE DATABASE sqla_test CHARACTER SET utf8mb4;
	GRANT ALL ON sqla_test.* TO 'rustmy'@'%';
	CREATE DATABASE test_schema CHARACTER SET utf8mb4;
	CREATE DATABASE test_schema_2 CHARACTER SET utf8mb4;
	GRANT ALL ON test_schema.* TO 'rustmy'@'%';
	GRANT ALL ON test_schema_2.* TO 'rustmy'@'%';
EOSQL

# Requires a client certificate signed by the test CA (gen-certs.sh).
docker_process_sql <<-'EOSQL'
	CREATE USER 'rustmy_x509'@'%' IDENTIFIED BY 'rustmy' REQUIRE X509;
	GRANT ALL ON rustmy_test.* TO 'rustmy_x509'@'%';
EOSQL

if [ "${native_available}" = "1" ]; then
	docker_process_sql <<-'EOSQL'
		CREATE USER 'rustmy_native'@'%' IDENTIFIED WITH mysql_native_password BY 'rustmy';
		GRANT ALL ON rustmy_test.* TO 'rustmy_native'@'%';
	EOSQL
fi
