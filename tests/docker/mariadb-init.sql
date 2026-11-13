-- Extra accounts for tests on MariaDB.
-- Requires a client certificate signed by the test CA (tests/docker/gen-certs.sh).
CREATE USER 'rustmy_x509'@'%' IDENTIFIED BY 'rustmy' REQUIRE X509;
GRANT ALL ON rustmy_test.* TO 'rustmy_x509'@'%';
