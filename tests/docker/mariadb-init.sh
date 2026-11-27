# Sourced by the MariaDB image entrypoint on first start; `docker_process_sql`
# runs SQL as root against the temporary init server.

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

# client_ed25519 (MariaDB 10.4+).
docker_process_sql <<-'EOSQL'
	INSTALL SONAME 'auth_ed25519';
	CREATE USER 'rustmy_ed25519'@'%' IDENTIFIED VIA ed25519 USING PASSWORD('rustmy');
	GRANT ALL ON rustmy_test.* TO 'rustmy_ed25519'@'%';
EOSQL

# PARSEC (MariaDB 11.6+); older servers lack the plugin, so failure is fine.
if docker_process_sql <<<"INSTALL SONAME 'auth_parsec'" 2>/dev/null; then
	docker_process_sql <<-'EOSQL'
		CREATE USER 'rustmy_parsec'@'%' IDENTIFIED VIA parsec USING PASSWORD('rustmy');
		GRANT ALL ON rustmy_test.* TO 'rustmy_parsec'@'%';
	EOSQL
fi

# mysql_clear_password, through PAM (compose.yaml loads the plugin and makes
# it ask for the password in cleartext); skipped if the plugin did not load.
if docker_process_sql <<<"SELECT plugin_status FROM information_schema.plugins WHERE plugin_name = 'pam'" | grep -q ACTIVE; then
	docker_process_sql <<-'EOSQL'
		CREATE USER 'rustmy_pam'@'%' IDENTIFIED VIA pam USING 'rustmy';
		GRANT ALL ON rustmy_test.* TO 'rustmy_pam'@'%';
	EOSQL
fi
