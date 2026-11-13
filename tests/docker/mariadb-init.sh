# Sourced by the MariaDB image entrypoint on first start; `docker_process_sql`
# runs SQL as root against the temporary init server.

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
