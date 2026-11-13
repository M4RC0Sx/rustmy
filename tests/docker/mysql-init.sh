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

if [ "${native_available}" = "1" ]; then
	docker_process_sql <<-'EOSQL'
		CREATE USER 'rustmy_native'@'%' IDENTIFIED WITH mysql_native_password BY 'rustmy';
		GRANT ALL ON rustmy_test.* TO 'rustmy_native'@'%';
	EOSQL
fi
