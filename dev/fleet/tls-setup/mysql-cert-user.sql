-- Fleet init: `xenon_cert` must present a client certificate (and its password).
create user 'xenon_cert'@'%' identified by 'xenon' require x509;
grant all on shop.* to 'xenon_cert'@'%';
