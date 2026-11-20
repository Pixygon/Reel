int dep_value(void);
unsigned long mpv_client_api_version(void){return ((unsigned long)dep_value()<<16)|5;}
