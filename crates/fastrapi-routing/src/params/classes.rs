crate::macros::define_param!(PyQuery, "Query");
crate::macros::define_param!(PyPath, "Path");
crate::macros::define_param!(PyCookie, "Cookie");
crate::macros::define_param!(PyHeader, "Header", header);
crate::macros::define_param!(PyBody, "Body", body);
crate::macros::define_param!(PyForm, "Form", media: "application/x-www-form-urlencoded");
crate::macros::define_param!(PyFile, "File", media: "multipart/form-data");

crate::macros::define_param!(PyDepends, "Depends");
crate::macros::define_param!(PySecurity, "Security");
